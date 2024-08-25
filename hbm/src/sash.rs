// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Constraint, CopyBuffer, CopyBufferImage, Layout};
use super::formats;
use super::types::{Error, Mapping, Modifier, Result, Size};
use super::utils;
use ash::vk;
use log::{debug, warn};
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::{cmp, ffi, num, ptr, slice, thread};

const REQUIRED_API_VERSION: u32 = vk::API_VERSION_1_1;

// TODO VK_KHR_external_semaphore_fd
#[derive(Clone, Copy)]
enum ExtId {
    KhrDriverProperties,
    KhrExternalMemoryFd,
    KhrImageFormatList,
    KhrMaintenance4,
    ExtExternalMemoryDmaBuf,
    ExtImageCompressionControl,
    ExtImageDrmFormatModifier,
    ExtPhysicalDeviceDrm,
    ExtQueueFamilyForeign,
    Count,
}

#[rustfmt::skip]
const EXT_TABLE: [(ExtId, &ffi::CStr, bool); ExtId::Count as usize] = [
    (ExtId::KhrDriverProperties,        ash::khr::driver_properties::NAME,          false),
    (ExtId::KhrExternalMemoryFd,        ash::khr::external_memory_fd::NAME,         true),
    (ExtId::KhrImageFormatList,         ash::khr::image_format_list::NAME,          false),
    (ExtId::KhrMaintenance4,            ash::khr::maintenance4::NAME,               true),
    (ExtId::ExtExternalMemoryDmaBuf,    ash::ext::external_memory_dma_buf::NAME,    true),
    (ExtId::ExtImageCompressionControl, ash::ext::image_compression_control::NAME,  false),
    (ExtId::ExtImageDrmFormatModifier,  ash::ext::image_drm_format_modifier::NAME,  false),
    (ExtId::ExtPhysicalDeviceDrm,       ash::ext::physical_device_drm::NAME,        false),
    (ExtId::ExtQueueFamilyForeign,      ash::ext::queue_family_foreign::NAME,       true),
];

fn has_api_version(ver: u32) -> Result<()> {
    let req_major = vk::api_version_major(REQUIRED_API_VERSION);
    let req_minor = vk::api_version_minor(REQUIRED_API_VERSION);

    if vk::api_version_major(ver) == req_major && vk::api_version_minor(ver) >= req_minor {
        Ok(())
    } else {
        Err(Error::NoSupport)
    }
}

fn has_device_id(props: vk::PhysicalDeviceDrmPropertiesEXT, dev_id: u64) -> Result<()> {
    if props.has_primary > 0 {
        let primary_id = utils::makedev(props.primary_major as u64, props.primary_minor as u64);
        if primary_id == dev_id {
            return Ok(());
        }
    }

    if props.has_render > 0 {
        let render_id = utils::makedev(props.render_major as u64, props.render_minor as u64);
        if render_id == dev_id {
            return Ok(());
        }
    }

    Err(Error::NoSupport)
}

fn can_export_import(props: vk::ExternalMemoryProperties) -> Result<()> {
    let flags =
        vk::ExternalMemoryFeatureFlags::EXPORTABLE | vk::ExternalMemoryFeatureFlags::IMPORTABLE;
    if props.external_memory_features.contains(flags) {
        Ok(())
    } else {
        Err(Error::NoSupport)
    }
}

struct Instance {
    // unused, but it keeps the library loaded
    _entry: ash::Entry,
    handle: ash::Instance,
}

unsafe extern "system" fn debug_utils_messenger(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut ffi::c_void,
) -> vk::Bool32 {
    let lv = match severity {
        vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE => log::Level::Debug,
        vk::DebugUtilsMessageSeverityFlagsEXT::INFO => log::Level::Info,
        vk::DebugUtilsMessageSeverityFlagsEXT::WARNING => log::Level::Warn,
        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR => log::Level::Error,
        _ => log::Level::Error,
    };

    // SAFETY: data is valid
    let data = unsafe { &*data };

    let msg_id = if !data.p_message_id_name.is_null() {
        // SAFETY: it is valid utf-8
        let cstr = unsafe { ffi::CStr::from_ptr(data.p_message_id_name) };
        Some(cstr.to_str().unwrap())
    } else {
        None
    };

    let msg = if !data.p_message.is_null() {
        // SAFETY: it is valid utf-8
        let cstr = unsafe { ffi::CStr::from_ptr(data.p_message) };
        Some(cstr.to_str().unwrap())
    } else {
        None
    };

    if msg_id.is_some() && msg.is_some() {
        log::log!(lv, "vulkan: {}: {}", msg_id.unwrap(), msg.unwrap());
    } else {
        let msg = msg_id.or(msg);
        if msg.is_some() {
            log::log!(lv, "vulkan: {}", msg.unwrap());
        }
    }

    vk::FALSE
}

impl Instance {
    fn new(app_name: &str, debug: bool) -> Result<Self> {
        let entry = Self::create_entry()?;
        let handle = Self::create_instance(&entry, app_name, debug)?;
        let instance = Self {
            _entry: entry,
            handle,
        };

        Ok(instance)
    }

    fn create_entry() -> Result<ash::Entry> {
        // SAFETY: we trust ash and the vulkan implementation
        let entry = unsafe { ash::Entry::load() }?;

        Ok(entry)
    }

    fn get_enabled_extensions(entry: &ash::Entry) -> Vec<*const ffi::c_char> {
        // SAFETY: entry is valid
        let exts = unsafe { entry.enumerate_instance_extension_properties(None) };
        let exts = exts.unwrap_or_default();

        let has_debug_utils = exts.iter().any(|ext| {
            // SAFETY: vk spec guarantees valid c-string
            let name = unsafe { ffi::CStr::from_ptr(ext.extension_name.as_ptr()) };
            name == ash::ext::debug_utils::NAME
        });

        if has_debug_utils {
            vec![ash::ext::debug_utils::NAME.as_ptr()]
        } else {
            Vec::new()
        }
    }

    fn create_instance(entry: &ash::Entry, app_name: &str, debug: bool) -> Result<ash::Instance> {
        // SAFETY: good
        let ver = unsafe { entry.try_enumerate_instance_version() }?;

        let ver = ver.unwrap_or(vk::API_VERSION_1_0);
        has_api_version(ver)?;

        let c_name = ffi::CString::new(app_name)?;
        let app_info = vk::ApplicationInfo::default()
            .application_name(&c_name)
            .api_version(REQUIRED_API_VERSION);
        let mut instance_info = vk::InstanceCreateInfo::default().application_info(&app_info);

        let mut enabled_exts = Vec::new();
        if debug {
            enabled_exts = Self::get_enabled_extensions(entry);
        }

        let mut msg_info = vk::DebugUtilsMessengerCreateInfoEXT::default();
        if debug && !enabled_exts.is_empty() {
            let msg_severity = vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO
                | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR;
            let msg_type = vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE;
            msg_info = msg_info
                .message_severity(msg_severity)
                .message_type(msg_type)
                .pfn_user_callback(Some(debug_utils_messenger));

            instance_info = instance_info
                .enabled_extension_names(&enabled_exts)
                .push_next(&mut msg_info);
        }

        // SAFETY: entry and instance_info are valid
        let handle = unsafe { entry.create_instance(&instance_info, None) }?;

        Ok(handle)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: handle is owned
        unsafe {
            self.handle.destroy_instance(None);
        }
    }
}

struct FormatProperties {
    format_class: &'static formats::FormatClass,
    modifiers: Vec<vk::DrmFormatModifierPropertiesEXT>,
}

#[derive(Default)]
struct DeviceCreateInfo {
    extensions: [bool; ExtId::Count as usize],
}

#[derive(Default)]
struct PhysicalDeviceProperties {
    ext_image_drm_format_modifier: bool,

    driver_id: vk::DriverId,
    max_image_dimension_2d: u32,
    max_uniform_buffer_range: u32,
    max_storage_buffer_range: u32,
    max_buffer_size: Size,

    protected_memory: bool,
    image_compression_control: bool,

    queue_family: u32,
    memory_types: Vec<vk::MemoryPropertyFlags>,

    formats: HashMap<vk::Format, FormatProperties>,

    external_memory_type: vk::ExternalMemoryHandleTypeFlags,
}

struct PhysicalDevice {
    instance: Instance,
    handle: vk::PhysicalDevice,

    properties: PhysicalDeviceProperties,
}

impl PhysicalDevice {
    fn new(
        instance: Instance,
        dev_idx: Option<usize>,
        dev_id: Option<u64>,
    ) -> Result<(Self, DeviceCreateInfo)> {
        let mut physical_dev = Self {
            instance,
            handle: Default::default(),
            properties: Default::default(),
        };

        let dev_info = physical_dev.init(dev_idx, dev_id)?;

        Ok((physical_dev, dev_info))
    }

    fn init(&mut self, dev_idx: Option<usize>, dev_id: Option<u64>) -> Result<DeviceCreateInfo> {
        // SAFETY: instance is valid
        let handles = unsafe { self.instance.handle.enumerate_physical_devices() }?;

        handles
            .into_iter()
            .enumerate()
            .find_map(|(idx, handle)| {
                if let Some(dev_idx) = dev_idx {
                    if dev_idx != idx {
                        return None;
                    }
                }

                self.probe(handle, dev_id).ok()
            })
            .ok_or(Error::InvalidParam)
    }

    fn probe(
        &mut self,
        handle: vk::PhysicalDevice,
        dev_id: Option<u64>,
    ) -> Result<DeviceCreateInfo> {
        // reset handle and properties
        self.handle = handle;
        self.properties = Default::default();

        let mut dev_info = Default::default();
        self.probe_extensions(dev_id, &mut dev_info)?;
        self.probe_properties(dev_id)?;
        self.probe_features()?;
        self.probe_queue_families()?;
        self.probe_memory_types()?;
        self.probe_formats()?;

        self.probe_external_memory()?;

        Ok(dev_info)
    }

    fn probe_extensions(
        &mut self,
        dev_id: Option<u64>,
        dev_info: &mut DeviceCreateInfo,
    ) -> Result<()> {
        // SAFETY: instance and handle are valid
        let exts = unsafe {
            self.instance
                .handle
                .enumerate_device_extension_properties(self.handle)
        }?;

        for (idx, ext) in EXT_TABLE.iter().enumerate() {
            let (id, name, required) = (ext.0, ext.1, ext.2);

            assert_eq!(id as usize, idx);

            dev_info.extensions[idx] = exts.iter().any(|ext| {
                // SAFETY: vk spec guarantees valid c-string
                let ext_name = unsafe { ffi::CStr::from_ptr(ext.extension_name.as_ptr()) };
                ext_name == name
            });

            if required && !dev_info.extensions[idx] {
                return Err(Error::NoSupport);
            }
        }

        if dev_id.is_some() && !dev_info.extensions[ExtId::ExtPhysicalDeviceDrm as usize] {
            return Err(Error::NoSupport);
        }

        self.properties.ext_image_drm_format_modifier =
            dev_info.extensions[ExtId::ExtImageDrmFormatModifier as usize];

        Ok(())
    }

    fn probe_properties(&mut self, dev_id: Option<u64>) -> Result<()> {
        let mut maint4_props = vk::PhysicalDeviceMaintenance4Properties::default();
        let mut drv_props = vk::PhysicalDeviceDriverProperties::default();
        let mut props = vk::PhysicalDeviceProperties2::default()
            .push_next(&mut maint4_props)
            .push_next(&mut drv_props);

        let mut drm_props = vk::PhysicalDeviceDrmPropertiesEXT::default();
        if dev_id.is_some() {
            props = props.push_next(&mut drm_props);
        }

        // SAFETY: handle and props are valid
        unsafe {
            self.instance
                .handle
                .get_physical_device_properties2(self.handle, &mut props);
        }

        let props = &props.properties;

        has_api_version(props.api_version)?;
        if let Some(dev_id) = dev_id {
            has_device_id(drm_props, dev_id)?;
        }

        self.properties.driver_id = drv_props.driver_id;

        if !self.properties.ext_image_drm_format_modifier {
            // If we have to go ahead without VK_EXT_image_drm_format_modifier,
            //
            //  - we will use OPAQUE_FD as the handle type, assuming it is actually dma-buf
            //  - we will limit VK_TILING_OPTIMAL support to non-planar formats
            //  - we will apply the scanout hack
            //  - we will violate VUID-vkGetImageSubresourceLayout-image-07790 for tiled images
            //  - on import, we may or may not violate
            //    - VUID-VkMemoryAllocateInfo-allocationSize-01742
            //    - VUID-VkMemoryDedicatedAllocateInfo-image-01878
            //    - VUID-VkMemoryDedicatedAllocateInfo-buffer-01879
            //
            // In other words, this is utterly wrong.
            //
            // TODO add modifiers to amdgpu gfx8
            if self.properties.driver_id == vk::DriverId::MESA_RADV {
                warn!("no VK_EXT_image_drm_format_modifier support");
            } else {
                return Err(Error::NoSupport);
            }
        }

        let limits = &props.limits;
        self.properties.max_image_dimension_2d = limits.max_image_dimension2_d;
        self.properties.max_uniform_buffer_range = limits.max_uniform_buffer_range;
        self.properties.max_storage_buffer_range = limits.max_storage_buffer_range;
        self.properties.max_buffer_size = maint4_props.max_buffer_size;

        Ok(())
    }

    fn probe_features(&mut self) -> Result<()> {
        let mut mem_prot_feats = vk::PhysicalDeviceProtectedMemoryFeatures::default();
        let mut img_comp_feats = vk::PhysicalDeviceImageCompressionControlFeaturesEXT::default();
        let mut feats = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut mem_prot_feats)
            .push_next(&mut img_comp_feats);

        // SAFETY: handle is valid
        unsafe {
            self.instance
                .handle
                .get_physical_device_features2(self.handle, &mut feats);
        }

        self.properties.protected_memory = mem_prot_feats.protected_memory > 0;
        self.properties.image_compression_control = img_comp_feats.image_compression_control > 0;

        Ok(())
    }

    fn probe_queue_families(&mut self) -> Result<()> {
        // SAFETY: handles are valid
        let props_list = unsafe {
            self.instance
                .handle
                .get_physical_device_queue_family_properties(self.handle)
        };

        let required_granularity = vk::Extent3D {
            width: 1,
            height: 1,
            depth: 1,
        };
        let required_flags = vk::QueueFlags::TRANSFER;

        self.properties.queue_family = props_list
            .into_iter()
            .enumerate()
            .find_map(|(idx, props)| {
                if props.min_image_transfer_granularity == required_granularity
                    && props.queue_flags.contains(required_flags)
                {
                    Some(idx as u32)
                } else {
                    None
                }
            })
            .ok_or(Error::NoSupport)?;

        Ok(())
    }

    fn probe_memory_types(&mut self) -> Result<()> {
        // SAFETY: handle is valid
        let props = unsafe {
            self.instance
                .handle
                .get_physical_device_memory_properties(self.handle)
        };

        self.properties.memory_types = props
            .memory_types_as_slice()
            .iter()
            .map(|mt| mt.property_flags)
            .collect();

        Ok(())
    }

    fn get_format_properties(
        &self,
        fmt: vk::Format,
        fmt_plane_count: u32,
    ) -> Vec<vk::DrmFormatModifierPropertiesEXT> {
        let mut mod_props_list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut props = vk::FormatProperties2::default().push_next(&mut mod_props_list);

        // SAFETY: valid
        unsafe {
            self.instance.handle.get_physical_device_format_properties2(
                self.handle,
                fmt,
                &mut props,
            );
        }

        if self.properties.ext_image_drm_format_modifier {
            let mod_count = mod_props_list.drm_format_modifier_count as usize;
            let mut mods = vec![Default::default(); mod_count];
            if mod_count == 0 {
                return mods;
            }

            let mut mod_props_list = vk::DrmFormatModifierPropertiesListEXT::default()
                .drm_format_modifier_properties(&mut mods);
            let mut props = vk::FormatProperties2::default().push_next(&mut mod_props_list);

            // SAFETY: valid
            unsafe {
                self.instance.handle.get_physical_device_format_properties2(
                    self.handle,
                    fmt,
                    &mut props,
                );
            }

            // vk::ImageAspectFlags supports up to 4 memory planes
            mods.into_iter()
                .filter(|mod_props| mod_props.drm_format_modifier_plane_count <= 4)
                .collect()
        } else {
            let linear_feats = props.format_properties.linear_tiling_features;
            let optimal_feats = props.format_properties.optimal_tiling_features;
            let mod_count = !linear_feats.is_empty() as usize + !optimal_feats.is_empty() as usize;
            let mut mods = Vec::with_capacity(mod_count);
            if mod_count == 0 {
                return mods;
            }

            if !linear_feats.is_empty() {
                let linear_props = vk::DrmFormatModifierPropertiesEXT {
                    drm_format_modifier: formats::MOD_LINEAR.0,
                    drm_format_modifier_plane_count: fmt_plane_count,
                    drm_format_modifier_tiling_features: linear_feats,
                };
                mods.push(linear_props);
            }
            // limit optimal tiling to non-planar formats
            if !optimal_feats.is_empty() && fmt_plane_count == 1 {
                let optimal_props = vk::DrmFormatModifierPropertiesEXT {
                    drm_format_modifier: formats::MOD_INVALID.0,
                    drm_format_modifier_plane_count: fmt_plane_count,
                    drm_format_modifier_tiling_features: optimal_feats,
                };
                mods.push(optimal_props);
            }

            mods
        }
    }

    fn probe_formats(&mut self) -> Result<()> {
        for drm_fmt in formats::KNOWN_FORMATS {
            /* some drm formats cannot be mapped */
            let fmt = formats::to_vk(drm_fmt);
            if fmt.is_err() {
                continue;
            }

            /* some drm formats map to the same vk formats */
            let fmt = fmt.unwrap().0;
            if self.properties.formats.contains_key(&fmt) {
                continue;
            }

            let fmt_class = formats::format_class(drm_fmt).unwrap();
            let mods = self.get_format_properties(fmt, fmt_class.plane_count as u32);

            let fmt_props = FormatProperties {
                format_class: fmt_class,
                modifiers: mods,
            };
            self.properties.formats.insert(fmt, fmt_props);
        }

        Ok(())
    }

    fn probe_external_memory(&mut self) -> Result<()> {
        self.properties.external_memory_type = if self.properties.ext_image_drm_format_modifier {
            vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT
        } else {
            // assume OPAQUE_FD is actually dma-buf
            vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD
        };

        Ok(())
    }
}

pub struct BufferInfo {
    pub flags: vk::BufferCreateFlags,
    pub usage: vk::BufferUsageFlags,
    pub external: bool,
}

pub struct BufferProperties {
    pub max_size: vk::DeviceSize,
}

pub struct ImageInfo {
    pub flags: vk::ImageCreateFlags,
    pub usage: vk::ImageUsageFlags,
    pub format: vk::Format,
    pub external: bool,
    pub no_compression: bool,
    pub scanout_hack: bool,
}

pub struct ImageProperties {
    pub max_extent: u32,
    pub modifiers: Vec<Modifier>,
}

#[derive(PartialEq)]
enum PipelineBarrierType {
    AcquireSrc,
    AcquireDst,
    ReleaseSrc,
    ReleaseDst,
}

struct PipelineBarrierScope {
    dependency_flags: vk::DependencyFlags,

    src_queue_family: u32,
    src_stage_mask: vk::PipelineStageFlags,
    src_access_mask: vk::AccessFlags,
    src_image_layout: vk::ImageLayout,

    dst_queue_family: u32,
    dst_stage_mask: vk::PipelineStageFlags,
    dst_access_mask: vk::AccessFlags,
    dst_image_layout: vk::ImageLayout,
}

// this is for scanout hack
#[repr(C)]
struct WsiImageCreateInfoMESA {
    s_type: vk::StructureType,
    p_next: *const ffi::c_void,
    scanout: bool,
    blit_src: bool,
    pad: [u64; 4],
}

impl Default for WsiImageCreateInfoMESA {
    fn default() -> Self {
        Self {
            s_type: vk::StructureType::from_raw(1000001002),
            p_next: ptr::null(),
            scanout: true,
            blit_src: false,
            pad: Default::default(),
        }
    }
}

// SAFETY: ok
unsafe impl vk::ExtendsPhysicalDeviceImageFormatInfo2 for WsiImageCreateInfoMESA {}
// SAFETY: ok
unsafe impl vk::ExtendsImageCreateInfo for WsiImageCreateInfoMESA {}

struct DeviceDispatch {
    memory: ash::khr::external_memory_fd::Device,
    modifier: ash::ext::image_drm_format_modifier::Device,
}

pub struct Device {
    physical_device: PhysicalDevice,
    handle: ash::Device,
    dispatch: DeviceDispatch,

    queue: vk::Queue,
    command_pool: CommandPool,
}

impl Device {
    pub fn build(
        name: &str,
        dev_idx: Option<usize>,
        dev_id: Option<u64>,
        debug: bool,
    ) -> Result<Arc<Device>> {
        debug!("initializing vulkan instance");
        let instance = Instance::new(name, debug)?;

        debug!("initializing vulkan physical device");
        let (physical_dev, dev_info) = PhysicalDevice::new(instance, dev_idx, dev_id)?;

        debug!("initializing vulkan device");
        let dev = Self::new(physical_dev, dev_info)?;

        Ok(Arc::new(dev))
    }

    // We might want to add a recreate fn to handle device lost.  Existing resources will keep the
    // old vk::Device alive, but gpu copies will no longer work for them.  We will also need to
    // check that resources have the same vk::Device handle as we do.
    fn new(physical_device: PhysicalDevice, dev_info: DeviceCreateInfo) -> Result<Self> {
        let handle = Self::create_device(&physical_device, dev_info)?;
        let dispatch = Self::create_dispatch(&handle, &physical_device);
        let mut dev = Self {
            physical_device,
            handle,
            dispatch,
            queue: Default::default(),
            command_pool: Default::default(),
        };

        dev.init()?;

        Ok(dev)
    }

    fn create_device(
        physical_dev: &PhysicalDevice,
        dev_info: DeviceCreateInfo,
    ) -> Result<ash::Device> {
        let props = &physical_dev.properties;

        let queue_prio = 1.0;
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(props.queue_family)
            .queue_priorities(slice::from_ref(&queue_prio));

        let enabled_exts: Vec<*const ffi::c_char> = dev_info
            .extensions
            .into_iter()
            .enumerate()
            .filter_map(|(idx, avail)| {
                if avail {
                    Some(EXT_TABLE[idx].1.as_ptr())
                } else {
                    None
                }
            })
            .collect();

        let mut mem_prot_feats = vk::PhysicalDeviceProtectedMemoryFeatures::default()
            .protected_memory(props.protected_memory);
        let mut img_comp_feats = vk::PhysicalDeviceImageCompressionControlFeaturesEXT::default()
            .image_compression_control(props.image_compression_control);
        let mut feats = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut mem_prot_feats)
            .push_next(&mut img_comp_feats);

        let dev_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(slice::from_ref(&queue_info))
            .enabled_extension_names(&enabled_exts)
            .push_next(&mut feats);

        // SAFETY: good
        let handle = unsafe {
            physical_dev
                .instance
                .handle
                .create_device(physical_dev.handle, &dev_info, None)
        }?;

        Ok(handle)
    }

    fn create_dispatch(handle: &ash::Device, physical_dev: &PhysicalDevice) -> DeviceDispatch {
        let instance_handle = &physical_dev.instance.handle;
        DeviceDispatch {
            memory: ash::khr::external_memory_fd::Device::new(instance_handle, handle),
            modifier: ash::ext::image_drm_format_modifier::Device::new(instance_handle, handle),
        }
    }

    fn init(&mut self) -> Result<()> {
        // SAFETY: good
        self.queue = unsafe {
            self.handle
                .get_device_queue(self.properties().queue_family, 0)
        };

        Ok(())
    }

    fn instance_handle(&self) -> &ash::Instance {
        &self.physical_device.instance.handle
    }

    fn properties(&self) -> &PhysicalDeviceProperties {
        &self.physical_device.properties
    }

    pub fn memory_plane_count(&self, fmt: vk::Format, modifier: Modifier) -> Result<u32> {
        let fmt_props = self
            .properties()
            .formats
            .get(&fmt)
            .ok_or(Error::NoSupport)?;

        fmt_props
            .modifiers
            .iter()
            .find_map(|mod_props| {
                if mod_props.drm_format_modifier == modifier.0 {
                    Some(mod_props.drm_format_modifier_plane_count)
                } else {
                    None
                }
            })
            .ok_or(Error::NoSupport)
    }

    fn format_plane_count(&self, fmt: vk::Format) -> u32 {
        let fmt_props = self.properties().formats.get(&fmt).unwrap();
        fmt_props.format_class.plane_count as u32
    }

    fn format_block_size(&self, fmt: vk::Format, plane: u32) -> u32 {
        let fmt_props = self.properties().formats.get(&fmt).unwrap();
        fmt_props.format_class.block_size[plane as usize] as u32
    }

    pub fn buffer_properties(&self, buf_info: BufferInfo) -> Result<BufferProperties> {
        if buf_info.flags.contains(vk::BufferCreateFlags::PROTECTED)
            && !self.properties().protected_memory
        {
            return Err(Error::NoSupport);
        }

        if buf_info.external {
            let external_info = vk::PhysicalDeviceExternalBufferInfo::default()
                .flags(buf_info.flags)
                .usage(buf_info.usage)
                .handle_type(self.properties().external_memory_type);
            let mut external_props = vk::ExternalBufferProperties::default();

            // SAFETY: good
            unsafe {
                self.instance_handle()
                    .get_physical_device_external_buffer_properties(
                        self.physical_device.handle,
                        &external_info,
                        &mut external_props,
                    );
            }

            can_export_import(external_props.external_memory_properties)?;
        }

        let mut max_size = self.properties().max_buffer_size;
        if buf_info
            .usage
            .contains(vk::BufferUsageFlags::UNIFORM_BUFFER)
        {
            max_size = cmp::min(max_size, self.properties().max_uniform_buffer_range as u64);
        }
        if buf_info
            .usage
            .contains(vk::BufferUsageFlags::STORAGE_BUFFER)
        {
            max_size = cmp::min(max_size, self.properties().max_storage_buffer_range as u64);
        }

        let props = BufferProperties { max_size };

        Ok(props)
    }

    fn get_image_tiling(&self, modifier: Modifier) -> vk::ImageTiling {
        if self.properties().ext_image_drm_format_modifier {
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
        } else if modifier == formats::MOD_LINEAR {
            vk::ImageTiling::LINEAR
        } else {
            vk::ImageTiling::OPTIMAL
        }
    }

    fn has_image_support(
        &self,
        img_info: &ImageInfo,
        compression: vk::ImageCompressionFlagsEXT,
        modifier: Modifier,
    ) -> Result<()> {
        let tiling = self.get_image_tiling(modifier);

        let mut comp_info = vk::ImageCompressionControlEXT::default().flags(compression);
        let mut fmt_info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(img_info.format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(tiling)
            .usage(img_info.usage)
            .flags(img_info.flags)
            .push_next(&mut comp_info);

        let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default();
        if img_info.external {
            external_info = external_info.handle_type(self.properties().external_memory_type);
            fmt_info = fmt_info.push_next(&mut external_info);
        }

        let mut mod_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default();
        if tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            mod_info = mod_info.drm_format_modifier(modifier.0);
            fmt_info = fmt_info.push_next(&mut mod_info);
        }

        let mut wsi_info = WsiImageCreateInfoMESA::default();
        if img_info.scanout_hack && tiling != vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            fmt_info = fmt_info.push_next(&mut wsi_info);
        }

        let mut comp_props = vk::ImageCompressionPropertiesEXT::default();
        let mut fmt_props = vk::ImageFormatProperties2::default().push_next(&mut comp_props);

        let mut external_props = vk::ExternalImageFormatProperties::default();
        if img_info.external {
            fmt_props = fmt_props.push_next(&mut external_props)
        }

        // SAFETY: ok
        unsafe {
            self.instance_handle()
                .get_physical_device_image_format_properties2(
                    self.physical_device.handle,
                    &fmt_info,
                    &mut fmt_props,
                )
        }?;

        if img_info.external {
            can_export_import(external_props.external_memory_properties)?;
        }

        if !comp_props.image_compression_flags.contains(compression) {
            return Err(Error::NoSupport);
        }

        Ok(())
    }

    pub fn image_properties(
        &self,
        img_info: ImageInfo,
        mut modifier: Modifier,
    ) -> Result<ImageProperties> {
        if img_info.flags.contains(vk::ImageCreateFlags::PROTECTED)
            && !self.properties().protected_memory
        {
            return Err(Error::NoSupport);
        }

        let mut compression = vk::ImageCompressionFlagsEXT::DEFAULT;
        if img_info.no_compression {
            if self.properties().image_compression_control {
                compression = vk::ImageCompressionFlagsEXT::DISABLED;
            } else if modifier.is_invalid() {
                modifier = formats::MOD_LINEAR;
            } else {
                return Err(Error::NoSupport);
            }
        }

        let mut required_feats = vk::FormatFeatureFlags::empty();
        if img_info.usage.contains(vk::ImageUsageFlags::SAMPLED) {
            required_feats |= vk::FormatFeatureFlags::SAMPLED_IMAGE;
        }
        if img_info.usage.contains(vk::ImageUsageFlags::STORAGE) {
            required_feats |= vk::FormatFeatureFlags::STORAGE_IMAGE;
        }
        if img_info
            .usage
            .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        {
            required_feats |= vk::FormatFeatureFlags::COLOR_ATTACHMENT;
        }

        let fmt_props = self
            .properties()
            .formats
            .get(&img_info.format)
            .ok_or(Error::NoSupport)?;

        // get supported modifiers
        let mut modifiers: Vec<Modifier> = fmt_props
            .modifiers
            .iter()
            .filter_map(|mod_props| {
                let candidate = Modifier(mod_props.drm_format_modifier);
                if !modifier.is_invalid() && candidate != modifier {
                    return None;
                }
                if !mod_props
                    .drm_format_modifier_tiling_features
                    .contains(required_feats)
                {
                    return None;
                }

                if self
                    .has_image_support(&img_info, compression, candidate)
                    .is_ok()
                {
                    Some(candidate)
                } else {
                    None
                }
            })
            .collect();

        if modifiers.is_empty() {
            return Err(Error::NoSupport);
        }

        // without modifier support, pick optimal when both are supported
        if !self.properties().ext_image_drm_format_modifier && modifiers.len() > 1 {
            modifiers = vec![formats::MOD_INVALID];
        }

        let props = ImageProperties {
            max_extent: self.properties().max_image_dimension_2d,
            modifiers,
        };

        Ok(props)
    }

    fn get_dma_buf_mt_mask(&self, dmabuf: BorrowedFd) -> u32 {
        // ignore self.properties().external_memory_type
        let external_memory_type = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        // SAFETY: ok
        let _ = unsafe {
            self.dispatch.memory.get_memory_fd_properties(
                external_memory_type,
                dmabuf.as_raw_fd(),
                &mut fd_props,
            )
        };

        fd_props.memory_type_bits
    }

    fn memory_types(&self, mt_mask: u32) -> Vec<(u32, vk::MemoryPropertyFlags)> {
        self.properties()
            .memory_types
            .iter()
            .enumerate()
            .filter_map(|(mt_idx, mt_flags)| {
                if mt_mask & (1 << mt_idx) != 0 {
                    Some((mt_idx as u32, *mt_flags))
                } else {
                    None
                }
            })
            .collect()
    }

    fn get_image_modifier(&self, handle: vk::Image, tiling: vk::ImageTiling) -> Result<Modifier> {
        let modifier = match tiling {
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT => {
                let mut mod_props = vk::ImageDrmFormatModifierPropertiesEXT::default();

                // SAFETY: good
                unsafe {
                    self.dispatch
                        .modifier
                        .get_image_drm_format_modifier_properties(handle, &mut mod_props)
                }?;

                Modifier(mod_props.drm_format_modifier)
            }
            vk::ImageTiling::LINEAR => formats::MOD_LINEAR,
            vk::ImageTiling::OPTIMAL => formats::MOD_INVALID,
            _ => unreachable!(),
        };

        Ok(modifier)
    }

    fn get_image_subresource_aspect(
        &self,
        tiling: vk::ImageTiling,
        mem_plane_count: u32,
        plane: u32,
    ) -> vk::ImageAspectFlags {
        match tiling {
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT => match plane {
                0 => vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
                1 => vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
                2 => vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
                3 => vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
                _ => unreachable!(),
            },
            // violate VUID-vkGetImageSubresourceLayout-image-07790 for vk::ImageTiling::OPTIMAL
            vk::ImageTiling::LINEAR | vk::ImageTiling::OPTIMAL => match plane {
                0 => {
                    if mem_plane_count > 1 {
                        vk::ImageAspectFlags::PLANE_0
                    } else {
                        vk::ImageAspectFlags::COLOR
                    }
                }
                1 => vk::ImageAspectFlags::PLANE_1,
                2 => vk::ImageAspectFlags::PLANE_2,
                _ => unreachable!(),
            },
            _ => unreachable!(),
        }
    }

    fn get_image_layout(
        &self,
        handle: vk::Image,
        tiling: vk::ImageTiling,
        fmt: vk::Format,
        modifier: Modifier,
        size: vk::DeviceSize,
    ) -> Layout {
        let mem_plane_count = self.memory_plane_count(fmt, modifier).unwrap();
        let mut layout = Layout::new()
            .size(size)
            .modifier(modifier)
            .plane_count(mem_plane_count);

        for plane in 0..mem_plane_count {
            let aspect = self.get_image_subresource_aspect(tiling, mem_plane_count, plane);
            let subres = vk::ImageSubresource::default().aspect_mask(aspect);

            // SAFETY: good
            let subres_layout = unsafe { self.handle.get_image_subresource_layout(handle, subres) };

            layout.offsets[plane as usize] = subres_layout.offset;
            layout.strides[plane as usize] = subres_layout.row_pitch;
        }

        layout
    }

    fn get_pipeline_barrier_scope(&self, ty: PipelineBarrierType) -> PipelineBarrierScope {
        // We assume all resources are owned by the foreign queue and, in the case of images, have
        // been initialized to the GENERAL layout.  Strictly speaking, the layout part is not
        // guaranteed unless we always explicitly transition the layout and release the ownership
        // during image creation.
        let src_queue_family;
        let src_stage_mask;
        let src_access_mask;
        let src_image_layout;
        let dst_queue_family;
        let dst_stage_mask;
        let dst_access_mask;
        let dst_image_layout;
        match ty {
            PipelineBarrierType::AcquireSrc | PipelineBarrierType::AcquireDst => {
                src_queue_family = vk::QUEUE_FAMILY_FOREIGN_EXT;
                src_stage_mask = vk::PipelineStageFlags::NONE;
                src_access_mask = vk::AccessFlags::NONE;
                src_image_layout = vk::ImageLayout::GENERAL;

                dst_queue_family = self.properties().queue_family;
                dst_stage_mask = vk::PipelineStageFlags::TRANSFER;
                if ty == PipelineBarrierType::AcquireSrc {
                    dst_access_mask = vk::AccessFlags::TRANSFER_READ;
                    dst_image_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
                } else {
                    dst_access_mask = vk::AccessFlags::TRANSFER_WRITE;
                    dst_image_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
                }
            }
            PipelineBarrierType::ReleaseSrc | PipelineBarrierType::ReleaseDst => {
                src_queue_family = self.properties().queue_family;
                if ty == PipelineBarrierType::ReleaseSrc {
                    src_stage_mask = vk::PipelineStageFlags::NONE;
                    src_access_mask = vk::AccessFlags::NONE;
                    src_image_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
                } else {
                    src_stage_mask = vk::PipelineStageFlags::TRANSFER;
                    src_access_mask = vk::AccessFlags::TRANSFER_WRITE;
                    src_image_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
                }

                dst_queue_family = vk::QUEUE_FAMILY_FOREIGN_EXT;
                dst_stage_mask = vk::PipelineStageFlags::NONE;
                dst_access_mask = vk::AccessFlags::NONE;
                dst_image_layout = vk::ImageLayout::GENERAL;
            }
        }

        PipelineBarrierScope {
            dependency_flags: vk::DependencyFlags::empty(),
            src_queue_family,
            src_stage_mask,
            src_access_mask,
            src_image_layout,
            dst_queue_family,
            dst_stage_mask,
            dst_access_mask,
            dst_image_layout,
        }
    }

    fn cmd_buffer_barrier(
        &self,
        cmd: vk::CommandBuffer,
        buf: vk::Buffer,
        scope: PipelineBarrierScope,
    ) {
        let buf_barrier = vk::BufferMemoryBarrier::default()
            .src_access_mask(scope.src_access_mask)
            .dst_access_mask(scope.dst_access_mask)
            .src_queue_family_index(scope.src_queue_family)
            .dst_queue_family_index(scope.dst_queue_family)
            .buffer(buf)
            .size(vk::WHOLE_SIZE);

        // SAFETY: good
        unsafe {
            self.handle.cmd_pipeline_barrier(
                cmd,
                scope.src_stage_mask,
                scope.dst_stage_mask,
                scope.dependency_flags,
                &[],
                slice::from_ref(&buf_barrier),
                &[],
            );
        }
    }

    fn cmd_image_barrier(
        &self,
        cmd: vk::CommandBuffer,
        img: vk::Image,
        aspect: vk::ImageAspectFlags,
        scope: PipelineBarrierScope,
    ) {
        let img_subres = vk::ImageSubresourceRange::default()
            .aspect_mask(aspect)
            .level_count(1)
            .layer_count(1);
        let img_barrier = vk::ImageMemoryBarrier::default()
            .src_access_mask(scope.src_access_mask)
            .dst_access_mask(scope.dst_access_mask)
            .old_layout(scope.src_image_layout)
            .new_layout(scope.dst_image_layout)
            .src_queue_family_index(scope.src_queue_family)
            .dst_queue_family_index(scope.dst_queue_family)
            .image(img)
            .subresource_range(img_subres);

        // SAFETY: good
        unsafe {
            self.handle.cmd_pipeline_barrier(
                cmd,
                scope.src_stage_mask,
                scope.dst_stage_mask,
                scope.dependency_flags,
                &[],
                &[],
                slice::from_ref(&img_barrier),
            );
        }
    }

    fn copy_buffer(&self, src: vk::Buffer, dst: vk::Buffer, region: vk::BufferCopy) -> Result<()> {
        let cmd = self.command_pool.begin(self)?;

        let src_acquire = self.get_pipeline_barrier_scope(PipelineBarrierType::AcquireSrc);
        let dst_acquire = self.get_pipeline_barrier_scope(PipelineBarrierType::AcquireDst);
        let src_release = self.get_pipeline_barrier_scope(PipelineBarrierType::ReleaseSrc);
        let dst_release = self.get_pipeline_barrier_scope(PipelineBarrierType::ReleaseDst);

        self.cmd_buffer_barrier(cmd, src, src_acquire);
        self.cmd_buffer_barrier(cmd, dst, dst_acquire);

        // SAFETY: good
        unsafe {
            self.handle
                .cmd_copy_buffer(cmd, src, dst, slice::from_ref(&region));
        }

        self.cmd_buffer_barrier(cmd, src, src_release);
        self.cmd_buffer_barrier(cmd, dst, dst_release);

        self.command_pool.end_and_submit(self, cmd)
    }

    fn copy_image_to_buffer(
        &self,
        img: vk::Image,
        buf: vk::Buffer,
        region: vk::BufferImageCopy,
    ) -> Result<()> {
        let cmd = self.command_pool.begin(self)?;

        let img_acquire = self.get_pipeline_barrier_scope(PipelineBarrierType::AcquireSrc);
        let buf_acquire = self.get_pipeline_barrier_scope(PipelineBarrierType::AcquireDst);
        let img_release = self.get_pipeline_barrier_scope(PipelineBarrierType::ReleaseSrc);
        let buf_release = self.get_pipeline_barrier_scope(PipelineBarrierType::ReleaseDst);
        let img_aspect = region.image_subresource.aspect_mask;
        let img_layout = img_acquire.dst_image_layout;

        self.cmd_image_barrier(cmd, img, img_aspect, img_acquire);
        self.cmd_buffer_barrier(cmd, buf, buf_acquire);

        // SAFETY: good
        unsafe {
            self.handle.cmd_copy_image_to_buffer(
                cmd,
                img,
                img_layout,
                buf,
                slice::from_ref(&region),
            );
        }

        self.cmd_image_barrier(cmd, img, img_aspect, img_release);
        self.cmd_buffer_barrier(cmd, buf, buf_release);

        self.command_pool.end_and_submit(self, cmd)
    }

    fn copy_buffer_to_image(
        &self,
        buf: vk::Buffer,
        img: vk::Image,
        region: vk::BufferImageCopy,
    ) -> Result<()> {
        let cmd = self.command_pool.begin(self)?;

        let buf_acquire = self.get_pipeline_barrier_scope(PipelineBarrierType::AcquireSrc);
        let img_acquire = self.get_pipeline_barrier_scope(PipelineBarrierType::AcquireDst);
        let buf_release = self.get_pipeline_barrier_scope(PipelineBarrierType::ReleaseSrc);
        let img_release = self.get_pipeline_barrier_scope(PipelineBarrierType::ReleaseDst);
        let img_aspect = region.image_subresource.aspect_mask;
        let img_layout = img_acquire.dst_image_layout;

        self.cmd_buffer_barrier(cmd, buf, buf_acquire);
        self.cmd_image_barrier(cmd, img, img_aspect, img_acquire);

        // SAFETY: good
        unsafe {
            self.handle.cmd_copy_buffer_to_image(
                cmd,
                buf,
                img,
                img_layout,
                slice::from_ref(&region),
            );
        }

        self.cmd_buffer_barrier(cmd, buf, buf_release);
        self.cmd_image_barrier(cmd, img, img_aspect, img_release);

        self.command_pool.end_and_submit(self, cmd)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.command_pool.clear(self);

        // SAFETY: handle is owned
        unsafe {
            self.handle.destroy_device(None);
        }
    }
}

#[derive(Default)]
struct CommandPool {
    pools: Mutex<HashMap<thread::ThreadId, (vk::CommandPool, vk::CommandBuffer)>>,
}

impl CommandPool {
    fn begin(&self, dev: &Device) -> Result<vk::CommandBuffer> {
        let cmd = self.get(dev)?;

        Self::begin_command_buffer(dev, cmd)?;

        Ok(cmd)
    }

    fn end_and_submit(&self, dev: &Device, cmd: vk::CommandBuffer) -> Result<()> {
        Self::end_command_buffer(dev, cmd)?;
        Self::queue_submit(dev, cmd)?;
        Self::queue_wait_idle(dev)?;
        Self::reset_command_buffer(dev, cmd)
    }

    fn get(&self, dev: &Device) -> Result<vk::CommandBuffer> {
        if let Some(cmd) = self.lookup() {
            return Ok(cmd);
        }

        let pool = Self::create_command_pool(dev)?;
        let cmd = Self::allocate_command_buffer(dev, pool)?; // TODO don't leak pool
                                                             //
        let mut pools = self.pools.lock().unwrap();
        pools.insert(thread::current().id(), (pool, cmd));

        Ok(cmd)
    }

    fn lookup(&self) -> Option<vk::CommandBuffer> {
        let pools = self.pools.lock().unwrap();
        pools.get(&thread::current().id()).map(|&(_, cmd)| cmd)
    }

    fn create_command_pool(dev: &Device) -> Result<vk::CommandPool> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(dev.properties().queue_family);

        // SAFETY:
        unsafe { dev.handle.create_command_pool(&pool_info, None) }.map_err(Error::from)
    }

    fn allocate_command_buffer(dev: &Device, pool: vk::CommandPool) -> Result<vk::CommandBuffer> {
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);

        // SAFETY: ok
        let cmds = unsafe { dev.handle.allocate_command_buffers(&alloc_info) }?;

        Ok(cmds[0])
    }

    fn begin_command_buffer(dev: &Device, cmd: vk::CommandBuffer) -> Result<()> {
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        // SAFETY: ok
        unsafe { dev.handle.begin_command_buffer(cmd, &begin_info) }.map_err(Error::from)
    }

    fn end_command_buffer(dev: &Device, cmd: vk::CommandBuffer) -> Result<()> {
        // SAFETY: ok
        unsafe { dev.handle.end_command_buffer(cmd) }.map_err(Error::from)
    }

    fn queue_submit(dev: &Device, cmd: vk::CommandBuffer) -> Result<()> {
        let submit_info = vk::SubmitInfo::default().command_buffers(slice::from_ref(&cmd));
        // SAFETY: ok
        unsafe {
            dev.handle
                .queue_submit(dev.queue, slice::from_ref(&submit_info), vk::Fence::null())
        }
        .map_err(Error::from)
    }

    fn queue_wait_idle(dev: &Device) -> Result<()> {
        // SAFETY: ok
        unsafe { dev.handle.queue_wait_idle(dev.queue) }.map_err(Error::from)
    }

    fn reset_command_buffer(dev: &Device, cmd: vk::CommandBuffer) -> Result<()> {
        // SAFETY: ok
        unsafe {
            dev.handle
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())
        }
        .map_err(Error::from)
    }

    fn clear(&self, dev: &Device) {
        let mut pools = self.pools.lock().unwrap();

        for (_, (pool, _)) in pools.drain() {
            // SAFETY: ok
            unsafe {
                dev.handle.destroy_command_pool(pool, None);
            }
        }
    }
}

pub struct Memory {
    device: Arc<Device>,
    handle: vk::DeviceMemory,
}

impl Memory {
    fn new(
        device: Arc<Device>,
        size: vk::DeviceSize,
        mt_idx: u32,
        dedicated_info: vk::MemoryDedicatedAllocateInfo,
        external: bool,
        dmabuf: Option<OwnedFd>,
    ) -> Result<Self> {
        let handle =
            Self::allocate_memory(&device, size, mt_idx, dedicated_info, external, dmabuf)?;
        let mem = Self { device, handle };

        Ok(mem)
    }

    fn with_buffer(buf: &Buffer, mt_idx: u32, dmabuf: Option<OwnedFd>) -> Result<Self> {
        let dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buf.handle);
        Self::new(
            buf.device.clone(),
            buf.size,
            mt_idx,
            dedicated_info,
            buf.external,
            dmabuf,
        )
    }

    fn with_image(img: &Image, mt_idx: u32, dmabuf: Option<OwnedFd>) -> Result<Self> {
        let dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(img.handle);
        Self::new(
            img.device.clone(),
            img.size,
            mt_idx,
            dedicated_info,
            img.external,
            dmabuf,
        )
    }

    fn allocate_memory(
        dev: &Device,
        size: vk::DeviceSize,
        mt_idx: u32,
        mut dedicated_info: vk::MemoryDedicatedAllocateInfo,
        external: bool,
        dmabuf: Option<OwnedFd>,
    ) -> Result<vk::DeviceMemory> {
        let mut mem_info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(mt_idx)
            .push_next(&mut dedicated_info);

        let mut export_info = vk::ExportMemoryAllocateInfo::default();
        if external {
            export_info = export_info.handle_types(dev.properties().external_memory_type);
            mem_info = mem_info.push_next(&mut export_info);
        }

        // VUID-VkImportMemoryFdInfoKHR-fd-00668 seems bogus
        let mut raw_fd: RawFd = -1;
        let mut import_info = vk::ImportMemoryFdInfoKHR::default();
        if let Some(dmabuf) = dmabuf {
            let mt_mask = dev.get_dma_buf_mt_mask(dmabuf.as_fd());
            if mt_mask & (1 << mt_idx) == 0 {
                return Err(Error::InvalidParam);
            }

            raw_fd = dmabuf.into_raw_fd();
            import_info = import_info
                .handle_type(dev.properties().external_memory_type)
                .fd(raw_fd);
            mem_info = mem_info.push_next(&mut import_info);
        }

        // SAFETY: good
        let handle = unsafe { dev.handle.allocate_memory(&mem_info, None) };

        let handle = handle.map_err(|err| {
            if raw_fd >= 0 {
                // SAFETY: close the opened raw fd
                unsafe {
                    OwnedFd::from_raw_fd(raw_fd);
                }
            }

            err
        })?;

        Ok(handle)
    }

    pub fn export_dma_buf(&self) -> Result<OwnedFd> {
        let fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(self.handle)
            .handle_type(self.device.properties().external_memory_type);

        // SAFETY: good
        let raw_fd = unsafe { self.device.dispatch.memory.get_memory_fd(&fd_info) }?;
        // SAFETY: ok
        let dmabuf = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        Ok(dmabuf)
    }

    pub fn map(&self, offset: vk::DeviceSize, size: vk::DeviceSize) -> Result<Mapping> {
        let flags = vk::MemoryMapFlags::empty();

        let len = num::NonZeroUsize::try_from(usize::try_from(size)?)?;

        // SAFETY: good
        let ptr = unsafe {
            self.device
                .handle
                .map_memory(self.handle, offset, size, flags)
        }?;
        let ptr = ptr::NonNull::new(ptr).unwrap();

        let mapping = Mapping { ptr, len };

        Ok(mapping)
    }

    pub fn unmap(&self) {
        // SAFETY: ok
        unsafe { self.device.handle.unmap_memory(self.handle) };
    }

    pub fn flush(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        let range = vk::MappedMemoryRange::default()
            .memory(self.handle)
            .offset(offset)
            .size(size);

        // SAFETY: ok
        let _ = unsafe {
            self.device
                .handle
                .flush_mapped_memory_ranges(slice::from_ref(&range))
        };
    }

    pub fn invalidate(&self, offset: vk::DeviceSize, size: vk::DeviceSize) {
        let range = vk::MappedMemoryRange::default()
            .memory(self.handle)
            .offset(offset)
            .size(size);

        // SAFETY: ok
        let _ = unsafe {
            self.device
                .handle
                .invalidate_mapped_memory_ranges(slice::from_ref(&range))
        };
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: handle is owned
        unsafe {
            self.device.handle.free_memory(self.handle, None);
        }
    }
}

pub struct Buffer {
    device: Arc<Device>,
    handle: vk::Buffer,

    size: vk::DeviceSize,
    mt_mask: u32,
    external: bool,

    memory: Option<Memory>,
}

impl Buffer {
    fn new(device: Arc<Device>, buf_info: BufferInfo, size: vk::DeviceSize) -> Result<Self> {
        let handle = Self::create_buffer(&device, &buf_info, size)?;
        let mut buf = Self {
            device,
            handle,
            size: 0,
            mt_mask: 0,
            external: buf_info.external,
            memory: None,
        };
        buf.init_memory_requirements();

        Ok(buf)
    }

    pub fn with_constraint(
        dev: Arc<Device>,
        buf_info: BufferInfo,
        size: vk::DeviceSize,
        con: Option<Constraint>,
    ) -> Result<Self> {
        let mut buf = Self::new(dev, buf_info, size)?;

        if let Some(con) = con {
            buf.size = buf.size.next_multiple_of(con.size_align);
        }

        Ok(buf)
    }

    pub fn with_layout(
        dev: Arc<Device>,
        buf_info: BufferInfo,
        size: vk::DeviceSize,
        layout: Layout,
        dmabuf: Option<BorrowedFd>,
    ) -> Result<Self> {
        let mut buf = Self::new(dev, buf_info, size)?;

        if buf.size > layout.size {
            return Err(Error::InvalidParam);
        }
        if let Some(dmabuf) = dmabuf {
            buf.mt_mask &= buf.device.get_dma_buf_mt_mask(dmabuf);
            if buf.mt_mask == 0 {
                return Err(Error::InvalidParam);
            }
        }

        Ok(buf)
    }

    fn create_buffer(
        dev: &Device,
        buf_info: &BufferInfo,
        size: vk::DeviceSize,
    ) -> Result<vk::Buffer> {
        let external = buf_info.external;

        let mut buf_info = vk::BufferCreateInfo::default()
            .flags(buf_info.flags)
            .size(size)
            .usage(buf_info.usage);

        let mut external_info = vk::ExternalMemoryBufferCreateInfo::default();
        if external {
            external_info = external_info.handle_types(dev.properties().external_memory_type);
            buf_info = buf_info.push_next(&mut external_info);
        }

        // SAFETY: good
        let handle = unsafe { dev.handle.create_buffer(&buf_info, None) }?;

        Ok(handle)
    }

    fn init_memory_requirements(&mut self) {
        let reqs_info = vk::BufferMemoryRequirementsInfo2::default().buffer(self.handle);
        let mut reqs = vk::MemoryRequirements2::default();

        // SAFETY: good
        unsafe {
            self.device
                .handle
                .get_buffer_memory_requirements2(&reqs_info, &mut reqs);
        }

        let reqs = reqs.memory_requirements;
        self.size = reqs.size;
        self.mt_mask = reqs.memory_type_bits;
    }

    pub fn layout(&self) -> Layout {
        Layout::new().size(self.size)
    }

    pub fn memory_types(&self) -> Vec<(u32, vk::MemoryPropertyFlags)> {
        self.device.memory_types(self.mt_mask)
    }

    pub fn bind_memory(&mut self, mt_idx: u32, dmabuf: Option<OwnedFd>) -> Result<()> {
        let mem = Memory::with_buffer(self, mt_idx, dmabuf)?;

        let bind_info = vk::BindBufferMemoryInfo::default()
            .buffer(self.handle)
            .memory(mem.handle);

        // SAFETY: ok
        unsafe {
            self.device
                .handle
                .bind_buffer_memory2(slice::from_ref(&bind_info))
        }
        .map_err(Error::from)?;

        self.memory = Some(mem);

        Ok(())
    }

    pub fn memory(&self) -> &Memory {
        self.memory.as_ref().unwrap()
    }

    pub fn copy_buffer(&self, src: &Buffer, copy: CopyBuffer) -> Result<()> {
        let region = vk::BufferCopy::default()
            .src_offset(copy.src_offset)
            .dst_offset(copy.dst_offset)
            .size(copy.size);

        self.device.copy_buffer(src.handle, self.handle, region)
    }

    pub fn copy_image(&self, src: &Image, copy: CopyBufferImage) -> Result<()> {
        let region = src.get_copy_region(copy);

        self.device
            .copy_image_to_buffer(src.handle, self.handle, region)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: handle is owned
        unsafe {
            self.device.handle.destroy_buffer(self.handle, None);
        }
    }
}

pub struct Image {
    device: Arc<Device>,
    handle: vk::Image,

    tiling: vk::ImageTiling,
    format: vk::Format,
    format_plane_count: u32,
    modifier: Modifier,

    size: vk::DeviceSize,
    mt_mask: u32,
    external: bool,

    memory: Option<Memory>,
}

impl Image {
    fn new(
        device: Arc<Device>,
        handle: vk::Image,
        tiling: vk::ImageTiling,
        format: vk::Format,
        external: bool,
    ) -> Result<Self> {
        let format_plane_count = device.format_plane_count(format);
        let mut img = Self {
            device,
            handle,
            tiling,
            format,
            format_plane_count,
            modifier: formats::MOD_INVALID,
            size: 0,
            mt_mask: 0,
            external,
            memory: None,
        };

        img.modifier = img.device.get_image_modifier(img.handle, tiling)?;
        img.init_memory_requirements();

        Ok(img)
    }

    pub fn with_constraint(
        dev: Arc<Device>,
        img_info: ImageInfo,
        width: u32,
        height: u32,
        modifiers: &[Modifier],
        con: Option<Constraint>,
    ) -> Result<Self> {
        let tiling = dev.get_image_tiling(modifiers[0]);
        let handle =
            Self::create_implicit_image(&dev, tiling, &img_info, width, height, modifiers)?;
        let mut img = Self::new(dev, handle, tiling, img_info.format, img_info.external)?;

        if let Some(con) = con {
            img.size = img.size.next_multiple_of(con.size_align);

            if tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
                // TODO fall back to explicit layout if constraint is not satisfied
            }
        }

        Ok(img)
    }

    pub fn with_layout(
        dev: Arc<Device>,
        img_info: ImageInfo,
        width: u32,
        height: u32,
        layout: Layout,
        dmabuf: Option<BorrowedFd>,
    ) -> Result<Self> {
        let tiling = dev.get_image_tiling(layout.modifier);
        let handle = if tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            Self::create_explicit_image(&dev, tiling, &img_info, width, height, layout)?
        } else {
            // ignore layout and hope for the best
            Self::create_implicit_image(
                &dev,
                tiling,
                &img_info,
                width,
                height,
                slice::from_ref(&layout.modifier),
            )?
        };
        let mut img = Self::new(dev, handle, tiling, img_info.format, img_info.external)?;

        if img.size > layout.size {
            return Err(Error::InvalidParam);
        }
        if let Some(dmabuf) = dmabuf {
            img.mt_mask &= img.device.get_dma_buf_mt_mask(dmabuf);
            if img.mt_mask == 0 {
                return Err(Error::InvalidParam);
            }
        }

        Ok(img)
    }

    fn create_implicit_image(
        dev: &Device,
        tiling: vk::ImageTiling,
        img_info: &ImageInfo,
        width: u32,
        height: u32,
        modifiers: &[Modifier],
    ) -> Result<vk::Image> {
        // make Modifier #[repr(transparent)]?
        let modifiers: Vec<u64> = modifiers.iter().map(|m| m.0).collect();
        let mod_info =
            vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(&modifiers);

        Self::create_image(dev, tiling, img_info, width, height, mod_info)
    }

    fn create_explicit_image(
        dev: &Device,
        tiling: vk::ImageTiling,
        img_info: &ImageInfo,
        width: u32,
        height: u32,
        layout: Layout,
    ) -> Result<vk::Image> {
        let count = layout.plane_count as usize;
        let mut plane_layouts = Vec::with_capacity(count);
        for plane in 0..count {
            let subres_layout = vk::SubresourceLayout::default()
                .offset(layout.offsets[plane])
                .row_pitch(layout.strides[plane]);
            plane_layouts.push(subres_layout);
        }

        let mod_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(layout.modifier.0)
            .plane_layouts(&plane_layouts);

        Self::create_image(dev, tiling, img_info, width, height, mod_info)
    }

    fn create_image<T: vk::ExtendsImageCreateInfo>(
        dev: &Device,
        tiling: vk::ImageTiling,
        img_info: &ImageInfo,
        width: u32,
        height: u32,
        mut mod_info: T,
    ) -> Result<vk::Image> {
        let external = img_info.external;
        let compression = if tiling == vk::ImageTiling::OPTIMAL && img_info.no_compression {
            vk::ImageCompressionFlagsEXT::DISABLED
        } else {
            vk::ImageCompressionFlagsEXT::DEFAULT
        };
        let scanout_hack = img_info.scanout_hack;

        let extent = vk::Extent3D {
            width,
            height,
            depth: 1,
        };

        let mut img_info = vk::ImageCreateInfo::default()
            .flags(img_info.flags)
            .image_type(vk::ImageType::TYPE_2D)
            .format(img_info.format)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(tiling)
            .usage(img_info.usage)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut mod_info);

        let mut external_info = vk::ExternalMemoryImageCreateInfo::default();
        if external {
            external_info = external_info.handle_types(dev.properties().external_memory_type);
            img_info = img_info.push_next(&mut external_info);
        }

        let mut comp_info = vk::ImageCompressionControlEXT::default();
        if compression != vk::ImageCompressionFlagsEXT::DEFAULT {
            comp_info = comp_info.flags(compression);
            img_info = img_info.push_next(&mut comp_info);
        }

        let mut wsi_info = WsiImageCreateInfoMESA::default();
        if scanout_hack && tiling != vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            img_info = img_info.push_next(&mut wsi_info);
        }

        // SAFETY: ok
        let handle = unsafe { dev.handle.create_image(&img_info, None) }?;

        Ok(handle)
    }

    fn init_memory_requirements(&mut self) {
        let reqs_info = vk::ImageMemoryRequirementsInfo2::default().image(self.handle);
        let mut reqs = vk::MemoryRequirements2::default();

        // SAFETY: good
        unsafe {
            self.device
                .handle
                .get_image_memory_requirements2(&reqs_info, &mut reqs);
        }

        let reqs = reqs.memory_requirements;
        self.size = reqs.size;
        self.mt_mask = reqs.memory_type_bits;
    }

    pub fn layout(&self) -> Layout {
        self.device.get_image_layout(
            self.handle,
            self.tiling,
            self.format,
            self.modifier,
            self.size,
        )
    }

    pub fn memory_types(&self) -> Vec<(u32, vk::MemoryPropertyFlags)> {
        self.device.memory_types(self.mt_mask)
    }

    pub fn bind_memory(&mut self, mt_idx: u32, dmabuf: Option<OwnedFd>) -> Result<()> {
        let mem = Memory::with_image(self, mt_idx, dmabuf)?;

        let bind_info = vk::BindImageMemoryInfo::default()
            .image(self.handle)
            .memory(mem.handle);

        // SAFETY: ok
        unsafe {
            self.device
                .handle
                .bind_image_memory2(slice::from_ref(&bind_info))
        }
        .map_err(Error::from)?;

        self.memory = Some(mem);

        Ok(())
    }

    pub fn memory(&self) -> &Memory {
        self.memory.as_ref().unwrap()
    }

    fn get_copy_region(&self, copy: CopyBufferImage) -> vk::BufferImageCopy {
        let aspect = match copy.plane {
            0 => {
                if self.format_plane_count > 1 {
                    vk::ImageAspectFlags::PLANE_0
                } else {
                    vk::ImageAspectFlags::COLOR
                }
            }
            1 => vk::ImageAspectFlags::PLANE_1,
            2 => vk::ImageAspectFlags::PLANE_2,
            _ => unreachable!(),
        };

        let bpp = self.device.format_block_size(self.format, copy.plane);
        let row_len = copy.stride as u32 / bpp;

        let subres = vk::ImageSubresourceLayers::default()
            .aspect_mask(aspect)
            .layer_count(1);
        let offset = vk::Offset3D::default().x(copy.x as i32).y(copy.y as i32);
        let extent = vk::Extent3D::default()
            .width(copy.width)
            .height(copy.height)
            .depth(1);

        vk::BufferImageCopy::default()
            .buffer_offset(copy.offset)
            .buffer_row_length(row_len)
            .image_subresource(subres)
            .image_offset(offset)
            .image_extent(extent)
    }

    pub fn copy_buffer(&self, src: &Buffer, copy: CopyBufferImage) -> Result<()> {
        let region = self.get_copy_region(copy);

        self.device
            .copy_buffer_to_image(src.handle, self.handle, region)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: handle is owned
        unsafe {
            self.device.handle.destroy_image(self.handle, None);
        }
    }
}
