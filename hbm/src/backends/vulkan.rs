// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{
    Class, Constraint, CopyBuffer, CopyBufferImage, Description, Extent, Flags, Handle,
    HandlePayload, Layout,
};
use crate::formats;
use crate::sash;
use crate::types::{Access, Error, Format, Mapping, Modifier, Result};
use crate::utils;
use ash::vk;
use log::info;
use std::os::fd::OwnedFd;
use std::sync::Arc;

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct Usage: u32 {
        const TRANSFER = 1 << 0;
        const UNIFORM = 1 << 1;
        const STORAGE = 1 << 2;
        const SAMPLED = 1 << 3;
        const COLOR = 1 << 4;
    }
}

fn get_usage(usage: super::Usage, valid_usage: Usage) -> Result<Usage> {
    let usage = match usage {
        super::Usage::Vulkan(usage) => usage,
        _ => return Err(Error::InvalidParam),
    };

    if !valid_usage.contains(usage) {
        return Err(Error::InvalidParam);
    }

    Ok(usage)
}

fn get_buffer_info(desc: Description, usage: super::Usage) -> Result<sash::BufferInfo> {
    let valid_usage = Usage::TRANSFER | Usage::UNIFORM | Usage::STORAGE;
    let usage = get_usage(usage, valid_usage)?;

    let mut buf_flags = vk::BufferCreateFlags::empty();
    let mut buf_usage = vk::BufferUsageFlags::empty();

    if desc.flags.contains(Flags::PROTECTED) {
        buf_flags |= vk::BufferCreateFlags::PROTECTED;
    }

    if desc.flags.contains(Flags::COPY) || usage.contains(Usage::TRANSFER) {
        buf_usage |= vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
    }
    if usage.contains(Usage::UNIFORM) {
        buf_usage |= vk::BufferUsageFlags::UNIFORM_BUFFER;
    }
    if usage.contains(Usage::STORAGE) {
        buf_usage |= vk::BufferUsageFlags::STORAGE_BUFFER;
    }

    // vulkan requires buf_usage to be non-empty
    if buf_usage.is_empty() {
        buf_usage |= vk::BufferUsageFlags::TRANSFER_SRC;
    }

    let buf_info = sash::BufferInfo {
        flags: buf_flags,
        usage: buf_usage,
    };

    Ok(buf_info)
}

fn get_image_info(desc: Description, usage: super::Usage) -> Result<sash::ImageInfo> {
    let valid_usage = Usage::TRANSFER | Usage::STORAGE | Usage::SAMPLED | Usage::COLOR;
    let usage = get_usage(usage, valid_usage)?;

    let mut img_flags = vk::ImageCreateFlags::empty();
    let mut img_usage = vk::ImageUsageFlags::empty();
    let (img_format, _) = formats::to_vk(desc.format)?;

    if desc.flags.contains(Flags::PROTECTED) {
        img_flags |= vk::ImageCreateFlags::PROTECTED;
    }

    if desc.flags.contains(Flags::COPY) || usage.contains(Usage::TRANSFER) {
        img_usage |= vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
    }
    if usage.contains(Usage::STORAGE) {
        img_usage |= vk::ImageUsageFlags::STORAGE;
    }
    if usage.contains(Usage::SAMPLED) {
        img_usage |= vk::ImageUsageFlags::SAMPLED;
    }
    if usage.contains(Usage::COLOR) {
        img_usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
    }

    // vulkan requires img_usage to be non-empty
    if img_usage.is_empty() {
        img_usage |= vk::ImageUsageFlags::TRANSFER_SRC;
    }

    let img_info = sash::ImageInfo {
        flags: img_flags,
        usage: img_usage,
        format: img_format,
        modifier: desc.modifier,
        no_compression: desc.flags.contains(Flags::NO_COMPRESSION),
        scanout_hack: desc.flags.contains(Flags::SCANOUT),
    };

    Ok(img_info)
}

fn get_memory_info(
    desc: Description,
    memory_types: Vec<(u32, vk::MemoryPropertyFlags)>,
) -> Result<sash::MemoryInfo> {
    let mut required_flags = vk::MemoryPropertyFlags::empty();
    let mut disallowed_flags = vk::MemoryPropertyFlags::empty();
    let mut optional_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
    let mut priority = 0.5;

    if desc.flags.contains(Flags::MAP) {
        required_flags |= vk::MemoryPropertyFlags::HOST_VISIBLE;

        if desc.flags.contains(Flags::COHERENT) {
            required_flags |= vk::MemoryPropertyFlags::HOST_COHERENT;
        }
        if !desc.flags.contains(Flags::NO_CACHE) {
            optional_flags |= vk::MemoryPropertyFlags::HOST_CACHED;
        }
    }

    if desc.flags.contains(Flags::PROTECTED) {
        required_flags |= vk::MemoryPropertyFlags::PROTECTED;
    } else {
        disallowed_flags |= vk::MemoryPropertyFlags::PROTECTED;
    }

    let mut mt_iter = memory_types.into_iter().filter(|(_, mt_flags)| {
        mt_flags.contains(required_flags) && !mt_flags.intersects(disallowed_flags)
    });

    let first_mt = mt_iter.next();
    if first_mt.is_none() {
        return Err(Error::NoSupport);
    }
    let best_mt = mt_iter.find(|(_, mt_flags)| mt_flags.contains(optional_flags));
    let mt_index = best_mt.or(first_mt).unwrap().0;

    if desc.flags.contains(Flags::PRIORITY_HIGH) {
        priority += 0.1;
    }

    let mem_info = sash::MemoryInfo { mt_index, priority };

    Ok(mem_info)
}

fn get_memory(handle: &Handle) -> Result<&sash::Memory> {
    let mem = match &handle.payload {
        HandlePayload::Buffer(buf) => buf.memory(),
        HandlePayload::Image(img) => img.memory(),
        _ => return Err(Error::NoSupport),
    };

    Ok(mem)
}

fn get_buffer(handle: &Handle) -> Result<&sash::Buffer> {
    let buf = match &handle.payload {
        HandlePayload::Buffer(buf) => buf,
        _ => return Err(Error::NoSupport),
    };

    Ok(buf)
}

fn get_image(handle: &Handle) -> Result<&sash::Image> {
    let img = match &handle.payload {
        HandlePayload::Image(img) => img,
        _ => return Err(Error::NoSupport),
    };

    Ok(img)
}

pub struct Backend {
    device: Arc<sash::Device>,
}

impl Backend {
    fn new(device_index: Option<usize>, device_id: Option<u64>, debug: bool) -> Result<Self> {
        let backend = Self {
            device: sash::Device::build("hbm", device_index, device_id, debug)?,
        };

        info!("vulkan backend initialized");

        Ok(backend)
    }
}

impl super::Backend for Backend {
    fn plane_count(&self, fmt: Format, modifier: Modifier) -> Result<u32> {
        let (fmt, _) = formats::to_vk(fmt)?;
        self.device.memory_plane_count(fmt, modifier)
    }

    fn classify(&self, desc: Description, usage: super::Usage) -> Result<Class> {
        let class = if desc.is_buffer() {
            let buf_info = get_buffer_info(desc, usage)?;
            let buf_props = self.device.buffer_properties(buf_info)?;

            Class::new(desc)
                .usage(usage)
                .max_extent(Extent::new_1d(buf_props.max_size))
        } else {
            let img_info = get_image_info(desc, usage)?;
            let img_props = self.device.image_properties(img_info)?;

            Class::new(desc)
                .usage(usage)
                .max_extent(Extent::new_2d(img_props.max_extent, img_props.max_extent))
                .modifiers(img_props.modifiers)
        };

        Ok(class)
    }

    fn allocate(&self, class: &Class, extent: Extent, con: Option<Constraint>) -> Result<Handle> {
        let handle = if class.description.is_buffer() {
            let buf_info = get_buffer_info(class.description, class.usage)?;
            let mut buf =
                sash::Buffer::with_size(self.device.clone(), buf_info, extent.size(), con)?;

            let mem_info = get_memory_info(class.description, buf.memory_types())?;
            buf.bind_memory(mem_info, None)?;

            Handle::new(HandlePayload::Buffer(buf))
        } else {
            let img_info = get_image_info(class.description, class.usage)?;

            let mut modifiers = &class.modifiers;
            let filtered_modifiers: Vec<Modifier>;
            if let Some(con) = &con {
                if !con.modifiers.is_empty() {
                    filtered_modifiers = modifiers
                        .iter()
                        .filter(|&m1| con.modifiers.iter().any(|m2| m2 == m1))
                        .copied()
                        .collect();
                    if filtered_modifiers.is_empty() {
                        return Err(Error::NoSupport);
                    }

                    modifiers = &filtered_modifiers;
                }
            }

            let mut img = sash::Image::with_modifiers(
                self.device.clone(),
                img_info,
                extent.width(),
                extent.height(),
                modifiers,
                con,
            )?;

            let mem_info = get_memory_info(class.description, img.memory_types())?;
            img.bind_memory(mem_info, None)?;

            Handle::new(HandlePayload::Image(img))
        };

        Ok(handle)
    }

    fn import_dma_buf(
        &self,
        class: &Class,
        extent: Extent,
        dmabuf: OwnedFd,
        layout: Layout,
    ) -> Result<Handle> {
        let handle = if class.description.is_buffer() {
            let buf_info = get_buffer_info(class.description, class.usage)?;
            let mut buf = sash::Buffer::with_dma_buf(
                self.device.clone(),
                buf_info,
                extent.size(),
                &dmabuf,
                layout,
            )?;

            let mem_info = get_memory_info(class.description, buf.memory_types())?;
            buf.bind_memory(mem_info, Some(dmabuf))?;

            Handle::new(HandlePayload::Buffer(buf))
        } else {
            let img_info = get_image_info(class.description, class.usage)?;
            let mut img = sash::Image::with_dma_buf(
                self.device.clone(),
                img_info,
                extent.width(),
                extent.height(),
                &dmabuf,
                layout,
            )?;

            let mem_info = get_memory_info(class.description, img.memory_types())?;
            img.bind_memory(mem_info, Some(dmabuf))?;

            Handle::new(HandlePayload::Image(img))
        };

        Ok(handle)
    }

    fn export_dma_buf(&self, handle: &Handle, name: Option<&str>) -> Result<OwnedFd> {
        let mem = get_memory(handle)?;
        let dmabuf = mem.export_dma_buf()?;

        if let Some(name) = name {
            let _ = utils::dma_buf_set_name(&dmabuf, name);
        }

        Ok(dmabuf)
    }

    fn layout(&self, handle: &Handle) -> Result<Layout> {
        let layout = match &handle.payload {
            HandlePayload::Buffer(buf) => buf.layout(),
            HandlePayload::Image(img) => img.layout(),
            _ => Err(Error::InvalidParam),
        }?;

        Ok(layout)
    }

    fn map(&self, handle: &Handle) -> Result<Mapping> {
        let mem = get_memory(handle)?;
        mem.map(0, vk::WHOLE_SIZE)
    }

    fn unmap(&self, handle: &Handle, _mapping: Mapping) {
        if let Ok(mem) = get_memory(handle) {
            mem.unmap();
        }
    }

    fn flush(&self, handle: &Handle) -> Result<()> {
        let mem = get_memory(handle)?;
        mem.flush(0, vk::WHOLE_SIZE)
    }

    fn invalidate(&self, handle: &Handle) -> Result<()> {
        let mem = get_memory(handle)?;
        mem.invalidate(0, vk::WHOLE_SIZE)
    }

    fn copy_buffer(
        &self,
        dst: &Handle,
        src: &Handle,
        copy: CopyBuffer,
        sync_fd: Option<OwnedFd>,
    ) -> Result<Option<OwnedFd>> {
        if let Some(sync_fd) = sync_fd {
            utils::poll(sync_fd, Access::Read)?;
        }

        let dst = get_buffer(dst)?;
        let src = get_buffer(src)?;
        dst.copy_buffer(src, copy)?;

        Ok(None)
    }

    fn copy_buffer_image(
        &self,
        dst: &Handle,
        src: &Handle,
        copy: CopyBufferImage,
        sync_fd: Option<OwnedFd>,
    ) -> Result<Option<OwnedFd>> {
        if let Some(sync_fd) = sync_fd {
            utils::poll(sync_fd, Access::Read)?;
        }

        if let HandlePayload::Buffer(_) = &dst.payload {
            let dst_buf = get_buffer(dst)?;
            let src_img = get_image(src)?;
            dst_buf.copy_image(src_img, copy)?
        } else {
            let dst_img = get_image(dst)?;
            let src_buf = get_buffer(src)?;
            dst_img.copy_buffer(src_buf, copy)?
        }

        Ok(None)
    }
}

#[derive(Default)]
pub struct Builder {
    device_index: Option<usize>,
    device_id: Option<u64>,
    debug: bool,
}

impl Builder {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn device_index(mut self, device_index: usize) -> Self {
        self.device_index = Some(device_index);
        self
    }

    // st_rdev
    pub fn device_id(mut self, device_id: u64) -> Self {
        self.device_id = Some(device_id);
        self
    }

    pub fn debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    pub fn build(mut self) -> Result<Backend> {
        match self.device_index.is_some() as i32 + self.device_id.is_some() as i32 {
            0 => {
                self.device_index = Some(0);
            }
            1 => (),
            _ => {
                return Err(Error::InvalidParam);
            }
        };

        Backend::new(self.device_index, self.device_id, self.debug)
    }
}
