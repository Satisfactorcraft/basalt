//! Vulkan instance, device, queue, surface and memory allocator.

use std::ffi::{c_void, CStr, CString};
use std::mem::ManuallyDrop;

use anyhow::{anyhow, bail, Context, Result};
use ash::{ext, khr, vk};
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use log::{debug, error, info, warn};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

pub struct GpuContext {
    /// Keeps the Vulkan loader library loaded.
    _entry: ash::Entry,
    pub instance: ash::Instance,
    debug: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    pub surface_loader: khr::surface::Instance,
    pub surface: vk::SurfaceKHR,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub swapchain_loader: khr::swapchain::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub allocator: ManuallyDrop<Allocator>,
    pub upload_pool: vk::CommandPool,
    pub max_anisotropy: Option<f32>,
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _ty: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut c_void,
) -> vk::Bool32 {
    let msg = if data.is_null() || (*data).p_message.is_null() {
        "<no message>".into()
    } else {
        CStr::from_ptr((*data).p_message).to_string_lossy()
    };
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        error!(target: "vulkan", "{msg}");
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        warn!(target: "vulkan", "{msg}");
    } else {
        debug!(target: "vulkan", "{msg}");
    }
    vk::FALSE
}

fn want_validation() -> bool {
    match std::env::var("BASALT_VALIDATION").as_deref() {
        Ok("0") => false,
        Ok(_) => true,
        Err(_) => cfg!(debug_assertions),
    }
}

impl GpuContext {
    pub fn new(window: &Window, app_name: &str) -> Result<Self> {
        let entry = unsafe { ash::Entry::load() }
            .context("could not load the Vulkan library, is a Vulkan driver installed?")?;

        let display = window.display_handle()?.as_raw();
        let mut extensions = ash_window::enumerate_required_extensions(display)?.to_vec();

        let available_layers = unsafe { entry.enumerate_instance_layer_properties()? };
        let has_validation = available_layers
            .iter()
            .any(|l| l.layer_name_as_c_str().is_ok_and(|n| n == VALIDATION_LAYER));
        let validation = want_validation() && has_validation;
        if want_validation() && !has_validation {
            warn!("Vulkan validation requested but VK_LAYER_KHRONOS_validation is not installed");
        }
        let mut layers = Vec::new();
        if validation {
            layers.push(VALIDATION_LAYER.as_ptr());
            extensions.push(ext::debug_utils::NAME.as_ptr());
        }

        let app_name = CString::new(app_name).unwrap_or_default();
        let app_info = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .application_version(1)
            .engine_name(c"basalt")
            .engine_version(1)
            .api_version(vk::API_VERSION_1_3);
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&extensions)
            .enabled_layer_names(&layers);
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .context("failed to create Vulkan instance (Vulkan 1.3 required)")?;

        // Everything below is created step by step; on failure, release
        // what exists in reverse order instead of leaking it.
        let mut debug = None;
        let mut surface = vk::SurfaceKHR::null();
        let surface_loader = khr::surface::Instance::new(&entry, &instance);
        let mut device: Option<ash::Device> = None;
        let result = (|| -> Result<Self> {
            if validation {
                let loader = ext::debug_utils::Instance::new(&entry, &instance);
                let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                    .message_severity(
                        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                            | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                    )
                    .message_type(
                        vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                            | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                            | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                    )
                    .pfn_user_callback(Some(debug_callback));
                let messenger = unsafe { loader.create_debug_utils_messenger(&info, None)? };
                info!("Vulkan validation layer enabled");
                debug = Some((loader, messenger));
            }

            surface = unsafe {
                ash_window::create_surface(
                    &entry,
                    &instance,
                    display,
                    window.window_handle()?.as_raw(),
                    None,
                )?
            };

            let (physical, queue_family) = pick_device(&instance, &surface_loader, surface)?;
            let props = unsafe { instance.get_physical_device_properties(physical) };
            let name = props
                .device_name_as_c_str()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            info!(
                "GPU: {name} (Vulkan {}.{}.{})",
                vk::api_version_major(props.api_version),
                vk::api_version_minor(props.api_version),
                vk::api_version_patch(props.api_version)
            );

            let base_features = unsafe { instance.get_physical_device_features(physical) };
            let anisotropy = base_features.sampler_anisotropy == vk::TRUE;
            let features = vk::PhysicalDeviceFeatures::default().sampler_anisotropy(anisotropy);
            let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
                .dynamic_rendering(true)
                .synchronization2(true);
            let priorities = [1.0];
            let queue_infos = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities)];
            let device_extensions = [khr::swapchain::NAME.as_ptr()];
            let device_info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queue_infos)
                .enabled_extension_names(&device_extensions)
                .enabled_features(&features)
                .push_next(&mut features13);
            let dev = unsafe { instance.create_device(physical, &device_info, None) }
                .context("failed to create Vulkan device")?;
            device = Some(dev.clone());
            let queue = unsafe { dev.get_device_queue(queue_family, 0) };
            let swapchain_loader = khr::swapchain::Device::new(&instance, &dev);

            let allocator = Allocator::new(&AllocatorCreateDesc {
                instance: instance.clone(),
                device: dev.clone(),
                physical_device: physical,
                debug_settings: Default::default(),
                buffer_device_address: false,
                allocation_sizes: Default::default(),
            })
            .map_err(|e| anyhow!("failed to create GPU allocator: {e}"))?;

            let upload_pool = unsafe {
                dev.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(queue_family)
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                    None,
                )?
            };

            Ok(Self {
                _entry: entry.clone(),
                instance: instance.clone(),
                debug: debug.take(),
                surface_loader: surface_loader.clone(),
                surface,
                physical,
                device: dev,
                swapchain_loader,
                queue,
                queue_family,
                allocator: ManuallyDrop::new(allocator),
                upload_pool,
                max_anisotropy: anisotropy.then_some(props.limits.max_sampler_anisotropy.min(16.0)),
            })
        })();

        if result.is_err() {
            unsafe {
                if let Some(dev) = device {
                    dev.destroy_device(None);
                }
                if surface != vk::SurfaceKHR::null() {
                    surface_loader.destroy_surface(surface, None);
                }
                if let Some((loader, messenger)) = debug {
                    loader.destroy_debug_utils_messenger(messenger, None);
                }
                instance.destroy_instance(None);
            }
        }
        result
    }

    pub fn wait_idle(&self) {
        unsafe {
            let _ = self.device.device_wait_idle();
        }
    }

    /// Records and runs a one-off command buffer, waiting for it to finish.
    /// Used for uploads at load time.
    pub fn one_shot(&self, record: impl FnOnce(vk::CommandBuffer)) -> Result<()> {
        unsafe {
            let cmd = self.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.upload_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let cmds = [cmd];
            let result = (|| -> ash::prelude::VkResult<()> {
                self.device.begin_command_buffer(
                    cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )?;
                record(cmd);
                self.device.end_command_buffer(cmd)?;
                let fence = self
                    .device
                    .create_fence(&vk::FenceCreateInfo::default(), None)?;
                let submit = vk::SubmitInfo::default().command_buffers(&cmds);
                let result = self
                    .device
                    .queue_submit(self.queue, &[submit], fence)
                    .and_then(|_| self.device.wait_for_fences(&[fence], true, u64::MAX));
                self.device.destroy_fence(fence, None);
                result
            })();
            self.device.free_command_buffers(self.upload_pool, &cmds);
            result?;
        }
        Ok(())
    }
}

impl Drop for GpuContext {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_command_pool(self.upload_pool, None);
            ManuallyDrop::drop(&mut self.allocator);
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            if let Some((loader, messenger)) = self.debug.take() {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

fn pick_device(
    instance: &ash::Instance,
    surface_loader: &khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32)> {
    let devices = unsafe { instance.enumerate_physical_devices()? };
    if devices.is_empty() {
        bail!("no Vulkan GPU found");
    }

    let mut best: Option<(u32, vk::PhysicalDevice, u32)> = None;
    for pd in devices {
        let props = unsafe { instance.get_physical_device_properties(pd) };
        if props.api_version < vk::API_VERSION_1_3 {
            continue;
        }
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f13);
        unsafe { instance.get_physical_device_features2(pd, &mut f2) };
        if f13.dynamic_rendering != vk::TRUE || f13.synchronization2 != vk::TRUE {
            continue;
        }
        let has_swapchain = unsafe { instance.enumerate_device_extension_properties(pd) }
            .unwrap_or_default()
            .iter()
            .any(|e| {
                e.extension_name_as_c_str()
                    .is_ok_and(|n| n == khr::swapchain::NAME)
            });
        if !has_swapchain {
            continue;
        }
        let families = unsafe { instance.get_physical_device_queue_family_properties(pd) };
        let family = families.iter().enumerate().find_map(|(i, f)| {
            let present = unsafe {
                surface_loader.get_physical_device_surface_support(pd, i as u32, surface)
            }
            .unwrap_or(false);
            (f.queue_flags.contains(vk::QueueFlags::GRAPHICS) && present).then_some(i as u32)
        });
        let Some(family) = family else { continue };
        let score = match props.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 4,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 3,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
            vk::PhysicalDeviceType::CPU => 1,
            _ => 0,
        };
        if best.is_none_or(|(s, _, _)| score > s) {
            best = Some((score, pd, family));
        }
    }
    best.map(|(_, pd, f)| (pd, f)).ok_or_else(|| {
        anyhow!("no GPU supports Vulkan 1.3 with dynamic rendering and presenting to this window")
    })
}
