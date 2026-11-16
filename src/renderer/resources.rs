//! GPU buffers and textures.

use anyhow::{anyhow, Result};
use ash::vk;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme};
use gpu_allocator::MemoryLocation;

use super::context::GpuContext;

pub struct Buffer {
    pub handle: vk::Buffer,
    pub allocation: Option<Allocation>,
}

impl Buffer {
    /// Mapped memory for CPU-visible buffers.
    pub fn mapped(&mut self) -> Option<&mut [u8]> {
        self.allocation.as_mut()?.mapped_slice_mut()
    }
}

pub struct Texture {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub allocation: Option<Allocation>,
}

pub struct Image2d {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl GpuContext {
    pub fn create_buffer(
        &mut self,
        size: u64,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
        name: &str,
    ) -> Result<Buffer> {
        unsafe {
            let handle = self.device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size.max(4))
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?;
            let requirements = self.device.get_buffer_memory_requirements(handle);
            let allocation = match self.allocator.allocate(&AllocationCreateDesc {
                name,
                requirements,
                location,
                linear: true,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            }) {
                Ok(a) => a,
                Err(e) => {
                    self.device.destroy_buffer(handle, None);
                    return Err(anyhow!("buffer allocation failed: {e}"));
                }
            };
            let buffer = Buffer {
                handle,
                allocation: Some(allocation),
            };
            let a = buffer.allocation.as_ref().unwrap();
            if let Err(e) = self
                .device
                .bind_buffer_memory(handle, a.memory(), a.offset())
            {
                self.destroy_buffer(buffer);
                return Err(e.into());
            }
            Ok(buffer)
        }
    }

    pub fn destroy_buffer(&mut self, mut buffer: Buffer) {
        unsafe { self.device.destroy_buffer(buffer.handle, None) };
        if let Some(a) = buffer.allocation.take() {
            let _ = self.allocator.free(a);
        }
    }

    /// Creates a GPU-only buffer filled with `data` via a staging copy.
    pub fn upload_buffer(
        &mut self,
        data: &[u8],
        usage: vk::BufferUsageFlags,
        name: &str,
    ) -> Result<Buffer> {
        let size = data.len() as u64;
        let mut staging = self.create_buffer(
            size,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
            "staging",
        )?;
        let Some(mapped) = staging.mapped() else {
            self.destroy_buffer(staging);
            return Err(anyhow!("staging not mapped"));
        };
        mapped[..data.len()].copy_from_slice(data);
        let buffer = match self.create_buffer(
            size,
            usage | vk::BufferUsageFlags::TRANSFER_DST,
            MemoryLocation::GpuOnly,
            name,
        ) {
            Ok(b) => b,
            Err(e) => {
                self.destroy_buffer(staging);
                return Err(e);
            }
        };
        let result = self.one_shot(|cmd| unsafe {
            self.device.cmd_copy_buffer(
                cmd,
                staging.handle,
                buffer.handle,
                &[vk::BufferCopy {
                    src_offset: 0,
                    dst_offset: 0,
                    size: size.max(4),
                }],
            );
        });
        self.destroy_buffer(staging);
        if let Err(e) = result {
            self.destroy_buffer(buffer);
            return Err(e);
        }
        Ok(buffer)
    }

    pub fn create_image(
        &mut self,
        info: &vk::ImageCreateInfo,
        aspect: vk::ImageAspectFlags,
        name: &str,
    ) -> Result<Texture> {
        unsafe {
            let image = self.device.create_image(info, None)?;
            let requirements = self.device.get_image_memory_requirements(image);
            let allocation = match self.allocator.allocate(&AllocationCreateDesc {
                name,
                requirements,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            }) {
                Ok(a) => a,
                Err(e) => {
                    self.device.destroy_image(image, None);
                    return Err(anyhow!("image allocation failed: {e}"));
                }
            };
            // From here on, failures clean up through destroy_texture
            // (destroying a null view is a no-op).
            let mut tex = Texture {
                image,
                view: vk::ImageView::null(),
                allocation: Some(allocation),
            };
            let a = tex.allocation.as_ref().unwrap();
            if let Err(e) = self.device.bind_image_memory(image, a.memory(), a.offset()) {
                self.destroy_texture(tex);
                return Err(e.into());
            }
            let view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(info.format)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: aspect,
                        base_mip_level: 0,
                        level_count: info.mip_levels,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
                None,
            );
            match view {
                Ok(view) => {
                    tex.view = view;
                    Ok(tex)
                }
                Err(e) => {
                    self.destroy_texture(tex);
                    Err(e.into())
                }
            }
        }
    }

    pub fn destroy_texture(&mut self, mut tex: Texture) {
        unsafe {
            self.device.destroy_image_view(tex.view, None);
            self.device.destroy_image(tex.image, None);
        }
        if let Some(a) = tex.allocation.take() {
            let _ = self.allocator.free(a);
        }
    }

    /// Uploads an RGBA8 image as a sampled texture with a full mip chain.
    pub fn upload_texture(&mut self, img: &Image2d, srgb: bool, name: &str) -> Result<Texture> {
        let format = if srgb {
            vk::Format::R8G8B8A8_SRGB
        } else {
            vk::Format::R8G8B8A8_UNORM
        };
        // Mips are generated with linear blits, which needs all three.
        let can_generate_mips = unsafe {
            self.instance
                .get_physical_device_format_properties(self.physical, format)
        }
        .optimal_tiling_features
        .contains(
            vk::FormatFeatureFlags::BLIT_SRC
                | vk::FormatFeatureFlags::BLIT_DST
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR,
        );
        let mip_levels = if can_generate_mips {
            32 - img.width.max(img.height).max(1).leading_zeros()
        } else {
            1
        };

        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: img.width,
                height: img.height,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let tex = self.create_image(&info, vk::ImageAspectFlags::COLOR, name)?;

        let mut staging = match self.create_buffer(
            img.rgba.len() as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
            "texture staging",
        ) {
            Ok(b) => b,
            Err(e) => {
                self.destroy_texture(tex);
                return Err(e);
            }
        };
        let Some(mapped) = staging.mapped() else {
            self.destroy_buffer(staging);
            self.destroy_texture(tex);
            return Err(anyhow!("staging not mapped"));
        };
        mapped[..img.rgba.len()].copy_from_slice(&img.rgba);

        let device = &self.device;
        let image = tex.image;
        let (w, h) = (img.width as i32, img.height as i32);
        let result = self.one_shot(|cmd| unsafe {
            image_barrier(
                device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                0,
                mip_levels,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::NONE,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::TRANSFER,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            device.cmd_copy_buffer_to_image(
                cmd,
                staging.handle,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: color_layers(0),
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: img.width,
                        height: img.height,
                        depth: 1,
                    },
                }],
            );

            let (mut mw, mut mh) = (w, h);
            for level in 1..mip_levels {
                // Previous level becomes the blit source.
                image_barrier(
                    device,
                    cmd,
                    image,
                    vk::ImageAspectFlags::COLOR,
                    level - 1,
                    1,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let (nw, nh) = ((mw / 2).max(1), (mh / 2).max(1));
                device.cmd_blit_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::ImageBlit {
                        src_subresource: color_layers(level - 1),
                        src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: mw, y: mh, z: 1 }],
                        dst_subresource: color_layers(level),
                        dst_offsets: [vk::Offset3D::default(), vk::Offset3D { x: nw, y: nh, z: 1 }],
                    }],
                    vk::Filter::LINEAR,
                );
                image_barrier(
                    device,
                    cmd,
                    image,
                    vk::ImageAspectFlags::COLOR,
                    level - 1,
                    1,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_READ,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
                (mw, mh) = (nw, nh);
            }
            image_barrier(
                device,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                mip_levels - 1,
                1,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::TRANSFER,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            );
        });
        self.destroy_buffer(staging);
        if let Err(e) = result {
            self.destroy_texture(tex);
            return Err(e);
        }
        Ok(tex)
    }
}

fn color_layers(mip: u32) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: mip,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn image_barrier(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    aspect: vk::ImageAspectFlags,
    base_mip: u32,
    mip_count: u32,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: base_mip,
            level_count: mip_count,
            base_array_layer: 0,
            layer_count: 1,
        });
    let barriers = [barrier];
    device.cmd_pipeline_barrier2(
        cmd,
        &vk::DependencyInfo::default().image_memory_barriers(&barriers),
    );
}
