//! Forward renderer: one lit, textured mesh pipeline using Vulkan 1.3
//! dynamic rendering. Models are loaded lazily by path the first time an
//! entity wants to draw them and cached until hot reload invalidates them.

mod context;
mod model;
mod resources;
mod swapchain;

use std::collections::HashMap;
use std::io::Cursor;

use anyhow::{anyhow, Result};
use ash::vk;
use glam::{Mat4, Vec3};
use gpu_allocator::MemoryLocation;
use log::{error, info};
use winit::window::Window;

use crate::scene::{Camera, Transform, World};
use crate::vfs::GameFs;
use context::GpuContext;
use model::{GpuMaterial, GpuModel, SamplerKey, Vertex};
use resources::{image_barrier, Buffer, Image2d, Texture};
use swapchain::Swapchain;

const FRAMES_IN_FLIGHT: usize = 2;
const MAX_MATERIALS: u32 = 4096;

static VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh.vert.spv"));
static FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mesh.frag.spv"));

#[repr(C)]
#[derive(Clone, Copy)]
struct FrameData {
    view_proj: [f32; 16],
    camera_pos: [f32; 4],
    sun_dir: [f32; 4],
    sun_color: [f32; 4],
    ambient: [f32; 4],
    /// x = 1 when the shader must gamma-encode (non-sRGB swapchain).
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DrawPush {
    model: [f32; 16],
    base_color: [f32; 4],
    /// x = alpha cutoff (negative: opaque, alpha ignored)
    material: [f32; 4],
}

struct Frame {
    cmd: vk::CommandBuffer,
    image_available: vk::Semaphore,
    in_flight: vk::Fence,
    ubo: Buffer,
    set: vk::DescriptorSet,
}

fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

pub struct Renderer {
    fs: GameFs,
    vsync: bool,
    window_size: (u32, u32),
    needs_resize: bool,
    swapchain: Option<Swapchain>,

    frame_layout: vk::DescriptorSetLayout,
    material_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    pipeline_formats: (vk::Format, vk::Format),
    descriptor_pool: vk::DescriptorPool,
    command_pool: vk::CommandPool,
    /// One sampler per distinct glTF sampler setting.
    samplers: HashMap<SamplerKey, vk::Sampler>,
    white: Option<Texture>,
    frames: Vec<Frame>,
    frame_index: usize,
    /// `None` = failed to load; not retried (or re-logged) until files change.
    models: HashMap<String, Option<GpuModel>>,

    // Dropped last, after Drop::drop has released everything above.
    ctx: GpuContext,
}

impl Renderer {
    pub fn new(window: &Window, fs: GameFs, title: &str, vsync: bool) -> Result<Self> {
        let ctx = GpuContext::new(window, title)?;
        let size = window.inner_size();
        // Start with null handles and fill them in. If any step fails,
        // dropping `renderer` releases exactly what was created so far
        // (destroying a null Vulkan handle is a no-op).
        let mut renderer = Self {
            fs,
            vsync,
            window_size: (size.width, size.height),
            needs_resize: false,
            swapchain: None,
            frame_layout: vk::DescriptorSetLayout::null(),
            material_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            pipeline_formats: (vk::Format::UNDEFINED, vk::Format::UNDEFINED),
            descriptor_pool: vk::DescriptorPool::null(),
            command_pool: vk::CommandPool::null(),
            samplers: HashMap::new(),
            white: None,
            frames: Vec::new(),
            frame_index: 0,
            models: HashMap::new(),
            ctx,
        };
        renderer.init()?;
        Ok(renderer)
    }

    fn init(&mut self) -> Result<()> {
        let device = self.ctx.device.clone();
        unsafe {
            let frame_bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)];
            self.frame_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&frame_bindings),
                None,
            )?;
            let material_bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            self.material_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&material_bindings),
                None,
            )?;
            let set_layouts = [self.frame_layout, self.material_layout];
            let push_ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
                .offset(0)
                .size(std::mem::size_of::<DrawPush>() as u32)];
            self.pipeline_layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&set_layouts)
                    .push_constant_ranges(&push_ranges),
                None,
            )?;

            let pool_sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: FRAMES_IN_FLIGHT as u32,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    descriptor_count: MAX_MATERIALS,
                },
            ];
            self.descriptor_pool = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .max_sets(MAX_MATERIALS + FRAMES_IN_FLIGHT as u32)
                    .pool_sizes(&pool_sizes),
                None,
            )?;

            self.command_pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(self.ctx.queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;

            self.white = Some(self.ctx.upload_texture(
                &Image2d {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                },
                true,
                "white",
            )?);

            // Command buffers and descriptor sets are freed with their pools.
            let cmds = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(FRAMES_IN_FLIGHT as u32),
            )?;
            let layouts = [self.frame_layout; FRAMES_IN_FLIGHT];
            let sets = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.descriptor_pool)
                    .set_layouts(&layouts),
            )?;
            for i in 0..FRAMES_IN_FLIGHT {
                let ubo = self.ctx.create_buffer(
                    std::mem::size_of::<FrameData>() as u64,
                    vk::BufferUsageFlags::UNIFORM_BUFFER,
                    MemoryLocation::CpuToGpu,
                    "frame ubo",
                )?;
                let buffer_info = [vk::DescriptorBufferInfo {
                    buffer: ubo.handle,
                    offset: 0,
                    range: vk::WHOLE_SIZE,
                }];
                device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet::default()
                        .dst_set(sets[i])
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                        .buffer_info(&buffer_info)],
                    &[],
                );
                // Owned by self.frames before the sync objects exist, so a
                // failure below still releases the buffer.
                self.frames.push(Frame {
                    cmd: cmds[i],
                    image_available: vk::Semaphore::null(),
                    in_flight: vk::Fence::null(),
                    ubo,
                    set: sets[i],
                });
                let frame = self.frames.last_mut().unwrap();
                frame.image_available =
                    device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
                frame.in_flight = device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?;
            }
        }
        self.recreate_swapchain()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.window_size = (width, height);
        self.needs_resize = true;
    }

    fn surface_extent_is_zero(&self) -> Result<bool> {
        let caps = unsafe {
            self.ctx
                .surface_loader
                .get_physical_device_surface_capabilities(self.ctx.physical, self.ctx.surface)?
        };
        Ok(caps.current_extent.width == 0 || caps.current_extent.height == 0)
    }

    fn recreate_swapchain(&mut self) -> Result<()> {
        self.ctx.wait_idle();
        let old = match self.swapchain.take() {
            Some(sc) => sc.destroy(&mut self.ctx),
            None => vk::SwapchainKHR::null(),
        };
        let result = Swapchain::new(
            &mut self.ctx,
            self.window_size.0,
            self.window_size.1,
            self.vsync,
            old,
        );
        if old != vk::SwapchainKHR::null() {
            unsafe { self.ctx.swapchain_loader.destroy_swapchain(old, None) };
        }
        let sc = result?;
        if self.pipeline_formats != (sc.format, sc.depth_format) {
            self.create_pipeline(sc.format, sc.depth_format)?;
        }
        self.swapchain = Some(sc);
        self.needs_resize = false;
        Ok(())
    }

    fn create_pipeline(&mut self, color: vk::Format, depth: vk::Format) -> Result<()> {
        let device = &self.ctx.device;
        unsafe {
            if self.pipeline != vk::Pipeline::null() {
                device.destroy_pipeline(self.pipeline, None);
                // Don't leave a dangling handle for Drop if creation fails.
                self.pipeline = vk::Pipeline::null();
                self.pipeline_formats = (vk::Format::UNDEFINED, vk::Format::UNDEFINED);
            }
            let vert_code = ash::util::read_spv(&mut Cursor::new(VERT_SPV))?;
            let frag_code = ash::util::read_spv(&mut Cursor::new(FRAG_SPV))?;
            let vert = device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&vert_code),
                None,
            )?;
            let frag = match device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&frag_code),
                None,
            ) {
                Ok(m) => m,
                Err(e) => {
                    device.destroy_shader_module(vert, None);
                    return Err(e.into());
                }
            };

            let stages = [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(vert)
                    .name(c"main"),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(frag)
                    .name(c"main"),
            ];
            let bindings = [vk::VertexInputBindingDescription {
                binding: 0,
                stride: std::mem::size_of::<Vertex>() as u32,
                input_rate: vk::VertexInputRate::VERTEX,
            }];
            let attributes = [
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32B32_SFLOAT,
                    offset: 12,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 24,
                },
            ];
            let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
                .vertex_binding_descriptions(&bindings)
                .vertex_attribute_descriptions(&attributes);
            let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
                .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
            let viewport = vk::PipelineViewportStateCreateInfo::default()
                .viewport_count(1)
                .scissor_count(1);
            let raster = vk::PipelineRasterizationStateCreateInfo::default()
                .polygon_mode(vk::PolygonMode::FILL)
                // Cull mode and front face are dynamic (set per draw).
                .line_width(1.0);
            let multisample = vk::PipelineMultisampleStateCreateInfo::default()
                .rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let depth_state = vk::PipelineDepthStencilStateCreateInfo::default()
                .depth_test_enable(true)
                .depth_write_enable(true)
                .depth_compare_op(vk::CompareOp::LESS);
            let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
                .color_write_mask(vk::ColorComponentFlags::RGBA)];
            let blend =
                vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
            let dynamic_states = [
                vk::DynamicState::VIEWPORT,
                vk::DynamicState::SCISSOR,
                vk::DynamicState::CULL_MODE,
                vk::DynamicState::FRONT_FACE,
            ];
            let dynamic =
                vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
            let color_formats = [color];
            let mut rendering = vk::PipelineRenderingCreateInfo::default()
                .color_attachment_formats(&color_formats)
                .depth_attachment_format(depth);

            let info = vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vertex_input)
                .input_assembly_state(&input_assembly)
                .viewport_state(&viewport)
                .rasterization_state(&raster)
                .multisample_state(&multisample)
                .depth_stencil_state(&depth_state)
                .color_blend_state(&blend)
                .dynamic_state(&dynamic)
                .layout(self.pipeline_layout)
                .push_next(&mut rendering);
            let result = device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None);
            device.destroy_shader_module(vert, None);
            device.destroy_shader_module(frag, None);
            self.pipeline = result.map_err(|(_, e)| anyhow!("pipeline creation failed: {e}"))?[0];
        }
        self.pipeline_formats = (color, depth);
        Ok(())
    }

    fn sampler(&mut self, key: SamplerKey) -> Result<vk::Sampler> {
        if let Some(s) = self.samplers.get(&key) {
            return Ok(*s);
        }
        let mut info = vk::SamplerCreateInfo::default()
            .mag_filter(key.mag)
            .min_filter(key.min)
            .mipmap_mode(key.mipmap)
            .address_mode_u(key.wrap_u)
            .address_mode_v(key.wrap_v)
            .address_mode_w(vk::SamplerAddressMode::REPEAT)
            // Non-mipmapped min filters sample the base level only.
            .max_lod(if key.use_mips {
                vk::LOD_CLAMP_NONE
            } else {
                0.0
            });
        let linear = key.min == vk::Filter::LINEAR && key.mag == vk::Filter::LINEAR;
        if let Some(a) = self.ctx.max_anisotropy.filter(|_| linear) {
            info = info.anisotropy_enable(true).max_anisotropy(a);
        }
        let sampler = unsafe { self.ctx.device.create_sampler(&info, None)? };
        self.samplers.insert(key, sampler);
        Ok(sampler)
    }

    // -- models -----------------------------------------------------------------

    /// Drops every cached model; they reload from disk on next use.
    pub fn invalidate_models(&mut self) {
        self.ctx.wait_idle();
        let pool = self.descriptor_pool;
        for (_, slot) in self.models.drain() {
            if let Some(m) = slot {
                m.destroy(&mut self.ctx, pool);
            }
        }
    }

    fn ensure_model(&mut self, path: &str) -> bool {
        if let Some(slot) = self.models.get(path) {
            return slot.is_some();
        }
        let slot = match self.load_model(path) {
            Ok(m) => {
                info!("loaded model '{path}' ({} primitives)", m.primitives.len());
                Some(m)
            }
            Err(e) => {
                error!("model '{path}': {e:#}");
                None
            }
        };
        let ok = slot.is_some();
        self.models.insert(path.to_string(), slot);
        ok
    }

    fn load_model(&mut self, path: &str) -> Result<GpuModel> {
        let full = self.fs.resolve(path)?;
        let cpu = model::load_gltf(&full)?;

        let ctx = &mut self.ctx;
        let vertex_buffer = ctx.upload_buffer(
            as_bytes(&cpu.vertices),
            vk::BufferUsageFlags::VERTEX_BUFFER,
            path,
        )?;
        let index_buffer = match ctx.upload_buffer(
            as_bytes(&cpu.indices),
            vk::BufferUsageFlags::INDEX_BUFFER,
            path,
        ) {
            Ok(b) => b,
            Err(e) => {
                ctx.destroy_buffer(vertex_buffer);
                return Err(e);
            }
        };
        let mut model = GpuModel {
            vertex_buffer,
            index_buffer,
            primitives: cpu.primitives,
            materials: Vec::new(),
            textures: Vec::new(),
        };
        let mut build = || -> Result<()> {
            for img in &cpu.images {
                model
                    .textures
                    .push(self.ctx.upload_texture(img, true, path)?);
            }
            for mat in &cpu.materials {
                let view = match mat.image {
                    Some(i) => model.textures[i].view,
                    None => self.white.as_ref().unwrap().view,
                };
                let set = unsafe {
                    let layouts = [self.material_layout];
                    self.ctx.device.allocate_descriptor_sets(
                        &vk::DescriptorSetAllocateInfo::default()
                            .descriptor_pool(self.descriptor_pool)
                            .set_layouts(&layouts),
                    )?[0]
                };
                let sampler = self.sampler(mat.sampler)?;
                let image_info = [vk::DescriptorImageInfo {
                    sampler,
                    image_view: view,
                    image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                }];
                unsafe {
                    self.ctx.device.update_descriptor_sets(
                        &[vk::WriteDescriptorSet::default()
                            .dst_set(set)
                            .dst_binding(0)
                            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                            .image_info(&image_info)],
                        &[],
                    );
                }
                model.materials.push(GpuMaterial {
                    set,
                    base_color: mat.base_color,
                    alpha_cutoff: mat.alpha_cutoff,
                    double_sided: mat.double_sided,
                });
            }
            Ok(())
        };
        if let Err(e) = build() {
            model.destroy(&mut self.ctx, self.descriptor_pool);
            return Err(e);
        }
        Ok(model)
    }

    // -- frame ------------------------------------------------------------------

    pub fn render(&mut self, world: &World) -> Result<()> {
        if self.window_size.0 == 0 || self.window_size.1 == 0 {
            return Ok(()); // minimized
        }
        if self.needs_resize || self.swapchain.is_none() {
            // The surface can report 0x0 (minimized) before we get the
            // Resized event; creating a swapchain then is invalid.
            if self.surface_extent_is_zero()? {
                return Ok(());
            }
            self.recreate_swapchain()?;
        }

        // Resolve what to draw (loading models on first use).
        let mut draws: Vec<(String, Mat4)> = Vec::new();
        for (_, e) in world.iter() {
            if !e.visible || e.pending_destroy {
                continue;
            }
            if let Some(path) = &e.model {
                if self.ensure_model(path) {
                    draws.push((path.clone(), e.transform.matrix()));
                }
            }
        }

        let device = self.ctx.device.clone();
        let frame = &self.frames[self.frame_index];
        let sc = self.swapchain.as_ref().unwrap();

        unsafe {
            device.wait_for_fences(&[frame.in_flight], true, u64::MAX)?;
            let image_index = match self.ctx.swapchain_loader.acquire_next_image(
                sc.handle,
                u64::MAX,
                frame.image_available,
                vk::Fence::null(),
            ) {
                Ok((i, _)) => i,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.needs_resize = true;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            device.reset_fences(&[frame.in_flight])?;

            // Per-frame uniforms.
            let extent = sc.extent;
            let aspect = extent.width as f32 / extent.height.max(1) as f32;
            let (cam_t, cam) = world.active_camera().unwrap_or_else(default_camera);
            let mut proj = Mat4::perspective_rh(cam.fov.to_radians(), aspect, cam.near, cam.far);
            proj.y_axis.y *= -1.0; // Vulkan clip space has +Y down
            let view = Mat4::from_rotation_translation(cam_t.rotation, cam_t.position).inverse();
            let env = &world.environment;
            let data = FrameData {
                view_proj: (proj * view).to_cols_array(),
                camera_pos: cam_t.position.extend(1.0).into(),
                sun_dir: env.sun_direction.extend(0.0).into(),
                sun_color: env.sun_color.extend(0.0).into(),
                ambient: env.ambient.extend(0.0).into(),
                params: [if sc.srgb { 0.0 } else { 1.0 }, 0.0, 0.0, 0.0],
            };
            let frame = &mut self.frames[self.frame_index];
            frame
                .ubo
                .mapped()
                .ok_or_else(|| anyhow!("ubo not mapped"))?[..std::mem::size_of::<FrameData>()]
                .copy_from_slice(as_bytes(&[data]));
            let frame = &self.frames[self.frame_index];

            let cmd = frame.cmd;
            device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            let image = sc.images[image_index as usize];
            image_barrier(
                &device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            );
            let depth_stages = vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS;
            image_barrier(
                &device,
                cmd,
                sc.depth.image,
                vk::ImageAspectFlags::DEPTH,
                0,
                1,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
                depth_stages,
                vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
                depth_stages,
                vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE
                    | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ,
            );

            let color_attachments = [vk::RenderingAttachmentInfo::default()
                .image_view(sc.views[image_index as usize])
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue {
                        // The clear bypasses the shader, so a non-sRGB
                        // swapchain needs the color encoded here.
                        float32: if sc.srgb {
                            env.clear_color
                        } else {
                            let [r, g, b, a] = env.clear_color;
                            [linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b), a]
                        },
                    },
                })];
            let depth_attachment = vk::RenderingAttachmentInfo::default()
                .image_view(sc.depth.view)
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: 1.0,
                        stencil: 0,
                    },
                });
            let render_area = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent,
            };
            device.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(render_area)
                    .layer_count(1)
                    .color_attachments(&color_attachments)
                    .depth_attachment(&depth_attachment),
            );

            device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(cmd, 0, &[render_area]);
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            device.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[frame.set],
                &[],
            );

            for (path, world_matrix) in &draws {
                let Some(Some(model)) = self.models.get(path) else {
                    continue;
                };
                device.cmd_bind_vertex_buffers(cmd, 0, &[model.vertex_buffer.handle], &[0]);
                device.cmd_bind_index_buffer(
                    cmd,
                    model.index_buffer.handle,
                    0,
                    vk::IndexType::UINT32,
                );
                for prim in &model.primitives {
                    let mat = &model.materials[prim.material.min(model.materials.len() - 1)];
                    device.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.pipeline_layout,
                        1,
                        &[mat.set],
                        &[],
                    );
                    let model_matrix = *world_matrix * prim.transform;
                    let cull = if mat.double_sided {
                        vk::CullModeFlags::NONE
                    } else {
                        vk::CullModeFlags::BACK
                    };
                    // A mirroring transform (negative determinant, e.g. a
                    // negative scale) reverses triangle winding.
                    let front = if model_matrix.determinant() < 0.0 {
                        vk::FrontFace::CLOCKWISE
                    } else {
                        vk::FrontFace::COUNTER_CLOCKWISE
                    };
                    device.cmd_set_cull_mode(cmd, cull);
                    device.cmd_set_front_face(cmd, front);
                    let push = DrawPush {
                        model: model_matrix.to_cols_array(),
                        base_color: mat.base_color.into(),
                        material: [mat.alpha_cutoff, 0.0, 0.0, 0.0],
                    };
                    device.cmd_push_constants(
                        cmd,
                        self.pipeline_layout,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        as_bytes(&[push]),
                    );
                    device.cmd_draw_indexed(cmd, prim.index_count, 1, prim.first_index, 0, 0);
                }
            }

            device.cmd_end_rendering(cmd);
            image_barrier(
                &device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::NONE,
                vk::AccessFlags2::NONE,
            );
            device.end_command_buffer(cmd)?;

            let wait = [frame.image_available];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let signal = [sc.render_finished[image_index as usize]];
            let cmds = [cmd];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&wait)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(&signal);
            device.queue_submit(self.ctx.queue, &[submit], frame.in_flight)?;

            let swapchains = [sc.handle];
            let indices = [image_index];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&signal)
                .swapchains(&swapchains)
                .image_indices(&indices);
            match self
                .ctx
                .swapchain_loader
                .queue_present(self.ctx.queue, &present)
            {
                Ok(false) => {}
                Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.needs_resize = true,
                Err(e) => return Err(e.into()),
            }
        }
        self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
        Ok(())
    }
}

/// The sRGB transfer function (same curve as the fragment shader).
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c < 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn default_camera() -> (Transform, Camera) {
    let position = Vec3::new(0.0, 3.0, 8.0);
    let (_, rotation, _) = Mat4::look_at_rh(position, Vec3::ZERO, Vec3::Y)
        .inverse()
        .to_scale_rotation_translation();
    (
        Transform {
            position,
            rotation,
            scale: Vec3::ONE,
        },
        Camera::default(),
    )
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.ctx.wait_idle();
        self.invalidate_models();
        if let Some(w) = self.white.take() {
            self.ctx.destroy_texture(w);
        }
        for frame in std::mem::take(&mut self.frames) {
            unsafe {
                self.ctx
                    .device
                    .destroy_semaphore(frame.image_available, None);
                self.ctx.device.destroy_fence(frame.in_flight, None);
            }
            self.ctx.destroy_buffer(frame.ubo);
        }
        if let Some(sc) = self.swapchain.take() {
            let handle = sc.destroy(&mut self.ctx);
            unsafe { self.ctx.swapchain_loader.destroy_swapchain(handle, None) };
        }
        unsafe {
            let d = &self.ctx.device;
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.pipeline_layout, None);
            d.destroy_descriptor_set_layout(self.frame_layout, None);
            d.destroy_descriptor_set_layout(self.material_layout, None);
            d.destroy_descriptor_pool(self.descriptor_pool, None);
            d.destroy_command_pool(self.command_pool, None);
            for (_, s) in self.samplers.drain() {
                d.destroy_sampler(s, None);
            }
        }
    }
}
