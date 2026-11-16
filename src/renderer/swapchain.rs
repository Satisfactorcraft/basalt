//! Swapchain plus the depth buffer that matches its size.

use anyhow::{anyhow, Result};
use ash::vk;
use log::info;

use super::context::GpuContext;
use super::resources::Texture;

pub struct Swapchain {
    pub handle: vk::SwapchainKHR,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    /// One per swapchain image, signaled when rendering to it is done.
    pub render_finished: Vec<vk::Semaphore>,
    pub depth: Texture,
    pub depth_format: vk::Format,
}

pub fn pick_depth_format(ctx: &GpuContext) -> Result<vk::Format> {
    [
        vk::Format::D32_SFLOAT,
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D16_UNORM,
    ]
    .into_iter()
    .find(|&f| {
        unsafe {
            ctx.instance
                .get_physical_device_format_properties(ctx.physical, f)
        }
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
    })
    .ok_or_else(|| anyhow!("no supported depth format"))
}

impl Swapchain {
    pub fn new(
        ctx: &mut GpuContext,
        width: u32,
        height: u32,
        vsync: bool,
        old: vk::SwapchainKHR,
    ) -> Result<Self> {
        let caps = unsafe {
            ctx.surface_loader
                .get_physical_device_surface_capabilities(ctx.physical, ctx.surface)?
        };
        let formats = unsafe {
            ctx.surface_loader
                .get_physical_device_surface_formats(ctx.physical, ctx.surface)?
        };
        let format = formats
            .iter()
            .find(|f| {
                matches!(
                    f.format,
                    vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB
                ) && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .or(formats.first())
            .copied()
            .ok_or_else(|| anyhow!("surface has no formats"))?;

        let modes = unsafe {
            ctx.surface_loader
                .get_physical_device_surface_present_modes(ctx.physical, ctx.surface)?
        };
        let present_mode = if vsync {
            vk::PresentModeKHR::FIFO
        } else if modes.contains(&vk::PresentModeKHR::MAILBOX) {
            vk::PresentModeKHR::MAILBOX
        } else if modes.contains(&vk::PresentModeKHR::IMMEDIATE) {
            vk::PresentModeKHR::IMMEDIATE
        } else {
            vk::PresentModeKHR::FIFO
        };

        let extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        // OPAQUE isn't guaranteed; some compositors only offer the others.
        let composite_alpha = [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
        .into_iter()
        .find(|&m| caps.supported_composite_alpha.contains(m))
        .ok_or_else(|| anyhow!("surface supports no composite alpha mode"))?;

        let mut image_count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            image_count = image_count.min(caps.max_image_count);
        }

        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(ctx.surface)
            .min_image_count(image_count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(composite_alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(old);
        let handle = unsafe { ctx.swapchain_loader.create_swapchain(&info, None)? };
        let images = unsafe { ctx.swapchain_loader.get_swapchain_images(handle)? };

        let mut views = Vec::with_capacity(images.len());
        let mut render_finished = Vec::with_capacity(images.len());
        for &image in &images {
            unsafe {
                views.push(
                    ctx.device.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(format.format)
                            .subresource_range(vk::ImageSubresourceRange {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                base_mip_level: 0,
                                level_count: 1,
                                base_array_layer: 0,
                                layer_count: 1,
                            }),
                        None,
                    )?,
                );
                render_finished.push(
                    ctx.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?,
                );
            }
        }

        let depth_format = pick_depth_format(ctx)?;
        let depth = ctx.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(depth_format)
                .extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            vk::ImageAspectFlags::DEPTH,
            "depth",
        )?;

        info!(
            "swapchain {}x{} {:?} {:?}, {} images",
            extent.width,
            extent.height,
            format.format,
            present_mode,
            images.len()
        );
        Ok(Self {
            handle,
            format: format.format,
            extent,
            images,
            views,
            render_finished,
            depth,
            depth_format,
        })
    }

    /// Destroys everything except the swapchain handle itself, which is
    /// returned so it can be passed as `old_swapchain` and then destroyed.
    pub fn destroy(self, ctx: &mut GpuContext) -> vk::SwapchainKHR {
        unsafe {
            for v in &self.views {
                ctx.device.destroy_image_view(*v, None);
            }
            for s in &self.render_finished {
                ctx.device.destroy_semaphore(*s, None);
            }
        }
        ctx.destroy_texture(self.depth);
        self.handle
    }
}
