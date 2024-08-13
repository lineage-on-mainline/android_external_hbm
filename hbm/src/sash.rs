// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Constraint, CopyBuffer, CopyBufferImage, Layout};
use super::formats;
use super::types::{Error, Mapping, Modifier, Result, Size};
use super::utils;
use ash::vk;
use log::debug;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::{cmp, ffi, num, ptr, slice, thread};

const REQUIRED_API_VERSION: u32 = vk::API_VERSION_1_1;

// TODO VK_KHR_external_semaphore_fd
#[derive(Clone, Copy)]
enum ExtId {
    KhrExternalMemoryFd,
    KhrImageFormatList,
    KhrMaintenance4,
    ExtExternalMemoryDmaBuf,
    ExtImageCompressionControl,
    ExtImageDrmFormatModifier,
    ExtMemoryPriority,
    ExtPhysicalDeviceDrm,
    ExtQueueFamilyForeign,
    Count,
}

#[rustfmt::skip]
const EXT_TABLE: [(ExtId, &ffi::CStr, bool); ExtId::Count as usize] = [
    (ExtId::KhrExternalMemoryFd,        ash::khr::external_memory_fd::NAME,         true),
    (ExtId::KhrImageFormatList,         ash::khr::image_format_list::NAME,          false),
    (ExtId::KhrMaintenance4,            ash::khr::maintenance4::NAME,               true),
    (ExtId::ExtExternalMemoryDmaBuf,    ash::ext::external_memory_dma_buf::NAME,    true),
    (ExtId::ExtImageCompressionControl, ash::ext::image_compression_control::NAME,  false),
    (ExtId::ExtImageDrmFormatModifier,  ash::ext::image_drm_format_modifier::NAME,  false),
    (ExtId::ExtMemoryPriority,          ash::ext::memory_priority::NAME,            false),
    (ExtId::ExtPhysicalDeviceDrm,       ash::ext::physical_device_drm::NAME,        false),
    (ExtId::ExtQueueFamilyForeign,      ash::ext::queue_family_foreign::NAME,       true),
];

const EXTERNAL_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

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

impl Instance {
    fn new(app_name: &str) -> Result<Self> {
        let entry = Self::create_entry()?;
        let handle = Self::create_instance(&entry, app_name)?;
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

    fn create_instance(entry: &ash::Entry, app_name: &str) -> Result<ash::Instance> {
        // SAFETY: good
        let ver = unsafe { entry.try_enumerate_instance_version() }?;

        let ver = ver.unwrap_or(vk::API_VERSION_1_0);
        has_api_version(ver)?;

        let c_name = ffi::CString::new(app_name)?;
        let app_info = vk::ApplicationInfo::default()
            .application_name(&c_name)
            .api_version(REQUIRED_API_VERSION);
        let instance_info = vk::InstanceCreateInfo::default().application_info(&app_info);

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

#[derive(Default)]
struct DeviceCreateInfo {
    extensions: [bool; ExtId::Count as usize],
}

#[derive(Default)]
struct PhysicalDeviceProperties {
    ext_image_drm_format_modifier: bool,

    max_image_dimension_2d: u32,
    max_storage_buffer_range: u32,
    max_buffer_size: Size,

    protected_memory: bool,
    memory_priority: bool,
    image_compression_control: bool,

    queue_family: u32,
    memory_types: Vec<vk::MemoryPropertyFlags>,

    formats: HashMap<vk::Format, Vec<vk::DrmFormatModifierPropertiesEXT>>,
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
        let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut maint4_props);

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

        let limits = &props.limits;
        self.properties.max_image_dimension_2d = limits.max_image_dimension2_d;
        self.properties.max_storage_buffer_range = limits.max_storage_buffer_range;
        self.properties.max_buffer_size = maint4_props.max_buffer_size;

        Ok(())
    }

    fn probe_features(&mut self) -> Result<()> {
        let mut mem_prot_feats = vk::PhysicalDeviceProtectedMemoryFeatures::default();
        let mut mem_prio_feats = vk::PhysicalDeviceMemoryPriorityFeaturesEXT::default();
        let mut img_comp_feats = vk::PhysicalDeviceImageCompressionControlFeaturesEXT::default();
        let mut feats = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut mem_prot_feats)
            .push_next(&mut mem_prio_feats)
            .push_next(&mut img_comp_feats);

        // SAFETY: handle is valid
        unsafe {
            self.instance
                .handle
                .get_physical_device_features2(self.handle, &mut feats);
        }

        self.properties.protected_memory = mem_prot_feats.protected_memory > 0;
        self.properties.memory_priority = mem_prio_feats.memory_priority > 0;
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
        format: vk::Format,
        format_plane_count: u32,
    ) -> Vec<vk::DrmFormatModifierPropertiesEXT> {
        let mut mod_props_list = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut props = vk::FormatProperties2::default().push_next(&mut mod_props_list);

        // SAFETY: valid
        unsafe {
            self.instance.handle.get_physical_device_format_properties2(
                self.handle,
                format,
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
                    format,
                    &mut props,
                );
            }

            mods
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
                    drm_format_modifier_plane_count: format_plane_count,
                    drm_format_modifier_tiling_features: linear_feats,
                };
                mods.push(linear_props);
            }
            if !optimal_feats.is_empty() {
                let optimal_props = vk::DrmFormatModifierPropertiesEXT {
                    drm_format_modifier: formats::MOD_INVALID.0,
                    drm_format_modifier_plane_count: format_plane_count,
                    drm_format_modifier_tiling_features: optimal_feats,
                };
                mods.push(optimal_props);
            }

            mods
        }
    }

    fn probe_formats(&mut self) -> Result<()> {
        for (fmt, format_plane_count) in formats::enumerate_vk() {
            let mods = self.get_format_properties(fmt, format_plane_count as u32);
            self.properties.formats.insert(fmt, mods);
        }

        Ok(())
    }
}

pub struct BufferInfo {
    pub flags: vk::BufferCreateFlags,
    pub usage: vk::BufferUsageFlags,
}

pub struct BufferProperties {
    pub max_size: vk::DeviceSize,
}

pub struct ImageInfo {
    pub flags: vk::ImageCreateFlags,
    pub usage: vk::ImageUsageFlags,
    pub format: vk::Format,
    pub modifier: Modifier,
    pub no_compression: bool,
}

pub struct ImageProperties {
    pub max_extent: u32,
    pub modifiers: Vec<Modifier>,
}

pub struct MemoryInfo {
    pub required_flags: vk::MemoryPropertyFlags,
    pub disallowed_flags: vk::MemoryPropertyFlags,
    pub optional_flags: vk::MemoryPropertyFlags,
    pub priority: f32,
}

#[derive(PartialEq)]
enum PipelineBarrierType {
    AcquireSrc,
    AcquireDst,
    ReleaseSrc,
    ReleaseDst,
}

pub struct PipelineBarrierScope {
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
    pub fn build(name: &str, dev_idx: Option<usize>, dev_id: Option<u64>) -> Result<Arc<Device>> {
        debug!("initializing vulkan instance");
        let instance = Instance::new(name)?;

        debug!("initializing vulkan physical device");
        let (physical_dev, dev_info) = PhysicalDevice::new(instance, dev_idx, dev_id)?;

        debug!("initializing vulkan device");
        let dev = Self::new(physical_dev, dev_info)?;

        Ok(Arc::new(dev))
    }

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
        let mut mem_prio_feats = vk::PhysicalDeviceMemoryPriorityFeaturesEXT::default()
            .memory_priority(props.memory_priority);
        let mut img_comp_feats = vk::PhysicalDeviceImageCompressionControlFeaturesEXT::default()
            .image_compression_control(props.image_compression_control);
        let mut feats = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut mem_prot_feats)
            .push_next(&mut mem_prio_feats)
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
        let mod_props_list = self
            .properties()
            .formats
            .get(&fmt)
            .ok_or(Error::NoSupport)?;

        mod_props_list
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

    pub fn buffer_properties(&self, buf_info: BufferInfo) -> Result<BufferProperties> {
        if buf_info.flags.contains(vk::BufferCreateFlags::PROTECTED)
            && !self.properties().protected_memory
        {
            return Err(Error::NoSupport);
        }

        let external_info = vk::PhysicalDeviceExternalBufferInfo::default()
            .flags(buf_info.flags)
            .usage(buf_info.usage)
            .handle_type(EXTERNAL_HANDLE_TYPE);
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

        let mut max_size = self.properties().max_buffer_size;
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
        flags: vk::ImageCreateFlags,
        usage: vk::ImageUsageFlags,
        compression: vk::ImageCompressionFlagsEXT,
        format: vk::Format,
        modifier: Modifier,
    ) -> Result<()> {
        let tiling = self.get_image_tiling(modifier);

        let mut external_info =
            vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(EXTERNAL_HANDLE_TYPE);
        let mut mod_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(modifier.0);
        let mut comp_info = vk::ImageCompressionControlEXT::default().flags(compression);
        let img_info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(tiling)
            .usage(usage)
            .flags(flags)
            .push_next(&mut external_info)
            .push_next(&mut mod_info)
            .push_next(&mut comp_info);

        let mut external_props = vk::ExternalImageFormatProperties::default();
        let mut comp_props = vk::ImageCompressionPropertiesEXT::default();
        let mut img_props = vk::ImageFormatProperties2::default()
            .push_next(&mut external_props)
            .push_next(&mut comp_props);

        // SAFETY: ok
        unsafe {
            self.instance_handle()
                .get_physical_device_image_format_properties2(
                    self.physical_device.handle,
                    &img_info,
                    &mut img_props,
                )
        }?;

        can_export_import(external_props.external_memory_properties)?;

        if !comp_props.image_compression_flags.contains(compression) {
            return Err(Error::NoSupport);
        }

        Ok(())
    }

    pub fn image_properties(&self, img_info: ImageInfo) -> Result<ImageProperties> {
        if img_info.flags.contains(vk::ImageCreateFlags::PROTECTED)
            && !self.properties().protected_memory
        {
            return Err(Error::NoSupport);
        }

        let mut compression = vk::ImageCompressionFlagsEXT::DEFAULT;
        let mut modifier = img_info.modifier;
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

        let mod_props_list = self
            .properties()
            .formats
            .get(&img_info.format)
            .ok_or(Error::NoSupport)?;

        // get supported modifiers
        let mut modifiers: Vec<Modifier> = mod_props_list
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
                    .has_image_support(
                        img_info.flags,
                        img_info.usage,
                        compression,
                        img_info.format,
                        candidate,
                    )
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

    fn get_image_subresource_aspect(
        &self,
        tiling: vk::ImageTiling,
        plane: u32,
    ) -> Result<vk::ImageAspectFlags> {
        let aspect = match tiling {
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT => match plane {
                0 => vk::ImageAspectFlags::MEMORY_PLANE_0_EXT,
                1 => vk::ImageAspectFlags::MEMORY_PLANE_1_EXT,
                2 => vk::ImageAspectFlags::MEMORY_PLANE_2_EXT,
                3 => vk::ImageAspectFlags::MEMORY_PLANE_3_EXT,
                _ => return Err(Error::NoSupport),
            },
            // we violate VUID-vkGetImageSubresourceLayout-image-07790
            vk::ImageTiling::LINEAR | vk::ImageTiling::OPTIMAL => match plane {
                0 => vk::ImageAspectFlags::PLANE_0,
                1 => vk::ImageAspectFlags::PLANE_1,
                2 => vk::ImageAspectFlags::PLANE_2,
                _ => return Err(Error::NoSupport),
            },
            _ => return Err(Error::NoSupport),
        };

        Ok(aspect)
    }

    fn find_mt(&self, mt_mask: u32, mem_info: &MemoryInfo) -> Result<u32> {
        let candidates: Vec<(usize, &vk::MemoryPropertyFlags)> = self
            .properties()
            .memory_types
            .iter()
            .enumerate()
            .filter(|(mt_idx, mt_flags)| {
                (mt_mask & (1 << mt_idx)) > 0
                    && mt_flags.contains(mem_info.required_flags)
                    && !mt_flags.intersects(mem_info.disallowed_flags)
            })
            .collect();
        if candidates.is_empty() {
            return Err(Error::NoSupport);
        }

        let mt_idx = if let Some(&(mt_idx, _)) = candidates
            .iter()
            .find(|(_, mt_flags)| mt_flags.contains(mem_info.optional_flags))
        {
            mt_idx
        } else {
            candidates[0].0
        };

        Ok(mt_idx as u32)
    }

    fn get_image_layout(
        &self,
        handle: vk::Image,
        tiling: vk::ImageTiling,
        format: vk::Format,
    ) -> Result<Layout> {
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
            _ => return Err(Error::NoSupport),
        };

        let mem_plane_count = self.memory_plane_count(format, modifier).unwrap();

        // note that size is not set here
        let mut layout = Layout::new()
            .modifier(modifier)
            .plane_count(mem_plane_count);
        for plane in 0..mem_plane_count {
            let aspect = self.get_image_subresource_aspect(tiling, plane)?;
            let subres = vk::ImageSubresource::default().aspect_mask(aspect);

            // SAFETY: good
            let subres_layout = unsafe { self.handle.get_image_subresource_layout(handle, subres) };

            layout.offsets[plane as usize] = subres_layout.offset;
            layout.strides[plane as usize] = subres_layout.row_pitch;
        }

        Ok(layout)
    }

    fn get_pipeline_barrier_scope(&self, ty: PipelineBarrierType) -> PipelineBarrierScope {
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
    size: vk::DeviceSize,
}

impl Memory {
    fn new(device: Arc<Device>) -> Self {
        Memory {
            device,
            handle: Default::default(),
            size: 0,
        }
    }

    fn get_buffer_requirements(&self, buf_handle: vk::Buffer) -> vk::MemoryRequirements {
        let reqs_info = vk::BufferMemoryRequirementsInfo2::default().buffer(buf_handle);
        let mut reqs = vk::MemoryRequirements2::default();

        // SAFETY: good
        unsafe {
            self.device
                .handle
                .get_buffer_memory_requirements2(&reqs_info, &mut reqs);
        }

        reqs.memory_requirements
    }

    fn init_with_buffer(
        &mut self,
        buf_handle: vk::Buffer,
        mem_info: MemoryInfo,
        size_align: vk::DeviceSize,
        import: Option<(OwnedFd, Layout)>,
    ) -> Result<()> {
        let reqs = self.get_buffer_requirements(buf_handle);
        let dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buf_handle);

        self.init_with_requirements(reqs, dedicated_info, mem_info, size_align, import)
    }

    fn get_image_requirements(&self, img_handle: vk::Image) -> vk::MemoryRequirements {
        let reqs_info = vk::ImageMemoryRequirementsInfo2::default().image(img_handle);
        let mut reqs = vk::MemoryRequirements2::default();

        // SAFETY: good
        unsafe {
            self.device
                .handle
                .get_image_memory_requirements2(&reqs_info, &mut reqs)
        };

        reqs.memory_requirements
    }

    fn init_with_image(
        &mut self,
        img_handle: vk::Image,
        mem_info: MemoryInfo,
        size_align: vk::DeviceSize,
        import: Option<(OwnedFd, Layout)>,
    ) -> Result<()> {
        let reqs = self.get_image_requirements(img_handle);
        let dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(img_handle);

        self.init_with_requirements(reqs, dedicated_info, mem_info, size_align, import)
    }

    fn get_dma_buf_mt_mask(&self, dmabuf: &OwnedFd) -> Result<u32> {
        let mut fd_props = vk::MemoryFdPropertiesKHR::default();
        // SAFETY: ok
        unsafe {
            self.device.dispatch.memory.get_memory_fd_properties(
                EXTERNAL_HANDLE_TYPE,
                dmabuf.as_raw_fd(),
                &mut fd_props,
            )
        }?;

        Ok(fd_props.memory_type_bits)
    }

    fn init_with_requirements(
        &mut self,
        reqs: vk::MemoryRequirements,
        dedicated_info: vk::MemoryDedicatedAllocateInfo,
        mem_info: MemoryInfo,
        size_align: vk::DeviceSize,
        import: Option<(OwnedFd, Layout)>,
    ) -> Result<()> {
        let size = reqs.size.next_multiple_of(size_align);

        let mut mt_mask = reqs.memory_type_bits;
        if let Some((ref dmabuf, layout)) = import {
            mt_mask &= self.get_dma_buf_mt_mask(dmabuf)?;
            if mt_mask == 0 || size > layout.size {
                return Err(Error::InvalidParam);
            }
        }
        let mt_index = self.device.find_mt(mt_mask, &mem_info)?;

        let dmabuf = import.map(|(dmabuf, _)| dmabuf);

        let handle =
            self.allocate_memory(size, mt_index, dedicated_info, mem_info.priority, dmabuf)?;
        self.handle = handle;
        self.size = size;

        Ok(())
    }

    fn allocate_memory(
        &mut self,
        size: vk::DeviceSize,
        mt_index: u32,
        mut dedicated_info: vk::MemoryDedicatedAllocateInfo,
        priority: f32,
        dmabuf: Option<OwnedFd>,
    ) -> Result<vk::DeviceMemory> {
        let mut export_info =
            vk::ExportMemoryAllocateInfo::default().handle_types(EXTERNAL_HANDLE_TYPE);
        let mut mem_info = vk::MemoryAllocateInfo::default()
            .allocation_size(size)
            .memory_type_index(mt_index)
            .push_next(&mut export_info)
            .push_next(&mut dedicated_info);

        let mut prio_info = vk::MemoryPriorityAllocateInfoEXT::default();
        if self.device.properties().memory_priority && priority != 0.5 {
            prio_info = prio_info.priority(priority);
            mem_info = mem_info.push_next(&mut prio_info);
        }

        // VUID-VkImportMemoryFdInfoKHR-fd-00668 seems bogus
        let mut raw_fd: RawFd = -1;
        let mut import_info = vk::ImportMemoryFdInfoKHR::default();
        if let Some(dmabuf) = dmabuf {
            raw_fd = dmabuf.into_raw_fd();
            import_info = import_info.handle_type(EXTERNAL_HANDLE_TYPE).fd(raw_fd);
            mem_info = mem_info.push_next(&mut import_info);
        }

        // SAFETY: good
        let handle = unsafe { self.device.handle.allocate_memory(&mem_info, None) };

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

    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    pub fn map(&self) -> Result<Mapping> {
        let offset = 0;
        let flags = vk::MemoryMapFlags::empty();

        let len = num::NonZeroUsize::try_from(usize::try_from(self.size)?)?;

        // SAFETY: good
        let ptr = unsafe {
            self.device
                .handle
                .map_memory(self.handle, offset, self.size, flags)
        }?;
        let ptr = ptr::NonNull::new(ptr).unwrap();

        let mapping = Mapping { ptr, len };

        Ok(mapping)
    }

    pub fn unmap(&self) {
        // SAFETY: ok
        unsafe { self.device.handle.unmap_memory(self.handle) };
    }

    pub fn flush(&self) -> Result<()> {
        let range = vk::MappedMemoryRange::default()
            .memory(self.handle)
            .size(self.size);

        // SAFETY: ok
        unsafe {
            self.device
                .handle
                .flush_mapped_memory_ranges(slice::from_ref(&range))
        }
        .map_err(Error::from)
    }

    pub fn invalidate(&self) -> Result<()> {
        let range = vk::MappedMemoryRange::default()
            .memory(self.handle)
            .size(self.size);

        // SAFETY: ok
        unsafe {
            self.device
                .handle
                .invalidate_mapped_memory_ranges(slice::from_ref(&range))
        }
        .map_err(Error::from)
    }

    pub fn export_dma_buf(&self) -> Result<OwnedFd> {
        let fd_info = vk::MemoryGetFdInfoKHR::default()
            .memory(self.handle)
            .handle_type(EXTERNAL_HANDLE_TYPE);

        // SAFETY: good
        let raw_fd = unsafe { self.device.dispatch.memory.get_memory_fd(&fd_info) }?;
        // SAFETY: ok
        let dmabuf = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        Ok(dmabuf)
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
    memory: Memory,
}

impl Buffer {
    pub fn new(
        dev: Arc<Device>,
        buf_info: BufferInfo,
        mem_info: MemoryInfo,
        size: vk::DeviceSize,
        con: Option<Constraint>,
    ) -> Result<Self> {
        let handle = Self::create_buffer(&dev, &buf_info, size)?;

        let (_, _, size_align) = Constraint::unpack(con);
        Self::with_handle(dev, handle, mem_info, size_align, None)
    }

    pub fn with_dma_buf(
        dev: Arc<Device>,
        buf_info: BufferInfo,
        mem_info: MemoryInfo,
        size: vk::DeviceSize,
        dmabuf: OwnedFd,
        layout: Layout,
    ) -> Result<Self> {
        let handle = Self::create_buffer(&dev, &buf_info, size)?;
        Self::with_handle(dev, handle, mem_info, 1, Some((dmabuf, layout)))
    }

    fn with_handle(
        device: Arc<Device>,
        handle: vk::Buffer,
        mem_info: MemoryInfo,
        size_align: vk::DeviceSize,
        import: Option<(OwnedFd, Layout)>,
    ) -> Result<Self> {
        let memory = Memory::new(device.clone());
        let mut buf = Self {
            device,
            handle,
            memory,
        };

        buf.memory
            .init_with_buffer(buf.handle, mem_info, size_align, import)?;
        buf.bind_memory()?;

        Ok(buf)
    }

    fn create_buffer(
        dev: &Device,
        buf_info: &BufferInfo,
        size: vk::DeviceSize,
    ) -> Result<vk::Buffer> {
        let mut external_info =
            vk::ExternalMemoryBufferCreateInfo::default().handle_types(EXTERNAL_HANDLE_TYPE);
        let buf_info = vk::BufferCreateInfo::default()
            .flags(buf_info.flags)
            .size(size)
            .usage(buf_info.usage)
            .push_next(&mut external_info);

        // SAFETY: good
        let handle = unsafe { dev.handle.create_buffer(&buf_info, None) }?;

        Ok(handle)
    }

    fn bind_memory(&self) -> Result<()> {
        let bind_info = vk::BindBufferMemoryInfo::default()
            .buffer(self.handle)
            .memory(self.memory.handle);

        // SAFETY: ok
        unsafe {
            self.device
                .handle
                .bind_buffer_memory2(slice::from_ref(&bind_info))
        }
        .map_err(Error::from)
    }

    pub fn layout(&self) -> Result<Layout> {
        let layout = Layout::new().size(self.memory.size());

        Ok(layout)
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    pub fn copy_buffer(&self, src: &Buffer, copy: CopyBuffer) -> Result<()> {
        // TODO validate
        let region = vk::BufferCopy::default()
            .src_offset(copy.src_offset)
            .dst_offset(copy.dst_offset)
            .size(copy.size);

        self.device.copy_buffer(src.handle, self.handle, region)
    }

    pub fn copy_image(&self, src: &Image, copy: CopyBufferImage) -> Result<()> {
        // TODO validate
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
    memory: Memory,

    tiling: vk::ImageTiling,
    format: vk::Format,
    format_plane_count: u32,
}

impl Image {
    pub fn new(
        dev: Arc<Device>,
        img_info: ImageInfo,
        mem_info: MemoryInfo,
        width: u32,
        height: u32,
        modifiers: &[Modifier],
        con: Option<Constraint>,
    ) -> Result<Self> {
        let tiling = dev.get_image_tiling(modifiers[0]);
        let handle =
            Self::create_implicit_image(&dev, tiling, &img_info, width, height, modifiers)?;

        // ignore constraints unless we can do explicit layout
        if con.is_some() && tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            // be careful not to leak handle
            let layout = dev.get_image_layout(handle, tiling, img_info.format);
            if layout.is_err() {
                // SAFETY: handle is owned
                unsafe {
                    dev.handle.destroy_image(handle, None);
                }
                return Err(Error::NoSupport);
            }

            // TODO fall back to explicit layout if constraint is not satisfied
        }

        let (_, _, size_align) = Constraint::unpack(con);
        Self::with_handle(
            dev,
            handle,
            tiling,
            img_info.format,
            mem_info,
            size_align,
            None,
        )
    }

    pub fn with_dma_buf(
        dev: Arc<Device>,
        img_info: ImageInfo,
        mem_info: MemoryInfo,
        width: u32,
        height: u32,
        dmabuf: OwnedFd,
        layout: Layout,
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

        Self::with_handle(
            dev,
            handle,
            tiling,
            img_info.format,
            mem_info,
            1,
            Some((dmabuf, layout)),
        )
    }

    fn with_handle(
        device: Arc<Device>,
        handle: vk::Image,
        tiling: vk::ImageTiling,
        format: vk::Format,
        mem_info: MemoryInfo,
        size_align: vk::DeviceSize,
        import: Option<(OwnedFd, Layout)>,
    ) -> Result<Self> {
        let memory = Memory::new(device.clone());
        let format_plane_count = formats::plane_count(formats::from_vk(format))?;
        let mut img = Self {
            device,
            handle,
            memory,
            tiling,
            format,
            format_plane_count,
        };

        img.memory
            .init_with_image(img.handle, mem_info, size_align, import)?;
        img.bind_memory()?;

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
        let compression = if tiling == vk::ImageTiling::OPTIMAL && img_info.no_compression {
            vk::ImageCompressionFlagsEXT::DISABLED
        } else {
            vk::ImageCompressionFlagsEXT::DEFAULT
        };

        let extent = vk::Extent3D {
            width,
            height,
            depth: 1,
        };

        let mut external_info =
            vk::ExternalMemoryImageCreateInfo::default().handle_types(EXTERNAL_HANDLE_TYPE);
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
            .push_next(&mut external_info)
            .push_next(&mut mod_info);

        let mut comp_info = vk::ImageCompressionControlEXT::default();
        if compression != vk::ImageCompressionFlagsEXT::DEFAULT {
            comp_info = comp_info.flags(compression);
            img_info = img_info.push_next(&mut comp_info);
        }

        // SAFETY: ok
        let handle = unsafe { dev.handle.create_image(&img_info, None) }?;

        Ok(handle)
    }

    fn bind_memory(&self) -> Result<()> {
        let bind_info = vk::BindImageMemoryInfo::default()
            .image(self.handle)
            .memory(self.memory.handle);

        // SAFETY: ok
        unsafe {
            self.device
                .handle
                .bind_image_memory2(slice::from_ref(&bind_info))
        }
        .map_err(Error::from)
    }

    pub fn layout(&self) -> Result<Layout> {
        let layout = self
            .device
            .get_image_layout(self.handle, self.tiling, self.format)?;
        Ok(layout.size(self.memory.size()))
    }

    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    fn get_copy_region(&self, copy: CopyBufferImage) -> vk::BufferImageCopy {
        let aspect = if self.format_plane_count == 1 {
            vk::ImageAspectFlags::COLOR
        } else {
            match copy.plane {
                1 => vk::ImageAspectFlags::PLANE_0,
                2 => vk::ImageAspectFlags::PLANE_1,
                3 => vk::ImageAspectFlags::PLANE_2,
                _ => unreachable!(),
            }
        };

        let bpp = formats::block_size(formats::from_vk(self.format), copy.plane).unwrap();
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
        // TODO validate
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
