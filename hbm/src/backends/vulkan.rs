// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{
    Class, Constraint, CopyBuffer, CopyBufferImage, Description, Extent, Handle, HandlePayload,
    Layout, MemoryFlags, ResourceFlags,
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

    if desc.flags.contains(ResourceFlags::PROTECTED) {
        buf_flags |= vk::BufferCreateFlags::PROTECTED;
    }

    if desc.flags.contains(ResourceFlags::COPY) || usage.contains(Usage::TRANSFER) {
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
    let (img_fmt, _) = formats::to_vk(desc.format)?;

    if desc.flags.contains(ResourceFlags::PROTECTED) {
        img_flags |= vk::ImageCreateFlags::PROTECTED;
    }

    if desc.flags.contains(ResourceFlags::COPY) || usage.contains(Usage::TRANSFER) {
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
        format: img_fmt,
        modifier: desc.modifier,
        no_compression: desc.flags.contains(ResourceFlags::NO_COMPRESSION),
        scanout_hack: desc.flags.contains(ResourceFlags::SCANOUT),
    };

    Ok(img_info)
}

fn get_memory_flags(memory_types: Vec<(u32, vk::MemoryPropertyFlags)>) -> Vec<MemoryFlags> {
    let known_mt_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL
        | vk::MemoryPropertyFlags::HOST_VISIBLE
        | vk::MemoryPropertyFlags::HOST_COHERENT
        | vk::MemoryPropertyFlags::HOST_CACHED;

    memory_types
        .into_iter()
        .map(|(_, mut mt_flags)| {
            mt_flags &= known_mt_flags;

            let mut flags = MemoryFlags::empty();
            if mt_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
                flags |= MemoryFlags::LOCAL;
            }
            if mt_flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
                flags |= MemoryFlags::MAPPABLE;
                if mt_flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT) {
                    flags |= MemoryFlags::COHERENT;
                }
                if mt_flags.contains(vk::MemoryPropertyFlags::HOST_CACHED) {
                    flags |= MemoryFlags::CACHED;
                }
            }

            flags
        })
        .collect()
}

fn find_mt(flags: MemoryFlags, memory_types: Vec<(u32, vk::MemoryPropertyFlags)>) -> Result<u32> {
    let known_mt_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL
        | vk::MemoryPropertyFlags::HOST_VISIBLE
        | vk::MemoryPropertyFlags::HOST_COHERENT
        | vk::MemoryPropertyFlags::HOST_CACHED;

    let mut required_flags = vk::MemoryPropertyFlags::empty();
    if flags.contains(MemoryFlags::LOCAL) {
        required_flags |= vk::MemoryPropertyFlags::DEVICE_LOCAL;
    }
    if flags.contains(MemoryFlags::MAPPABLE) {
        required_flags |= vk::MemoryPropertyFlags::HOST_VISIBLE;
        if flags.contains(MemoryFlags::COHERENT) {
            required_flags |= vk::MemoryPropertyFlags::HOST_COHERENT;
        }
        if flags.contains(MemoryFlags::CACHED) {
            required_flags |= vk::MemoryPropertyFlags::HOST_CACHED;
        }
    }

    let mut mt_iter = memory_types.into_iter().filter(|(_, mt_flags)| {
        mt_flags.contains(required_flags)
    });

    let first_mt = mt_iter.next();
    if first_mt.is_none() {
        return Err(Error::InvalidParam);
    }
    let best_mt = mt_iter.find(|(_, mt_flags)| (*mt_flags & known_mt_flags) == required_flags);
    let mt_idx = best_mt.or(first_mt).unwrap().0;

    Ok(mt_idx)
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

    fn with_constraint(
        &self,
        class: &Class,
        extent: Extent,
        con: Option<Constraint>,
    ) -> Result<Handle> {
        let handle = if class.description.is_buffer() {
            let buf_info = get_buffer_info(class.description, class.usage)?;
            let buf = sash::Buffer::with_size(self.device.clone(), buf_info, extent.size(), con)?;

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

            let img = sash::Image::with_modifiers(
                self.device.clone(),
                img_info,
                extent.width(),
                extent.height(),
                modifiers,
                con,
            )?;

            Handle::new(HandlePayload::Image(img))
        };

        Ok(handle)
    }

    fn with_layout(&self, class: &Class, extent: Extent, layout: Layout) -> Result<Handle> {
        let handle = if class.description.is_buffer() {
            let buf_info = get_buffer_info(class.description, class.usage)?;
            let buf =
                sash::Buffer::with_layout(self.device.clone(), buf_info, extent.size(), layout)?;

            Handle::new(HandlePayload::Buffer(buf))
        } else {
            let img_info = get_image_info(class.description, class.usage)?;
            let img = sash::Image::with_layout(
                self.device.clone(),
                img_info,
                extent.width(),
                extent.height(),
                layout,
            )?;

            Handle::new(HandlePayload::Image(img))
        };

        Ok(handle)
    }

    fn memory_types(&self, handle: &Handle, dmabuf: Option<&OwnedFd>) -> Vec<MemoryFlags> {
        let memory_types = match handle.payload {
            HandlePayload::Buffer(ref buf) => buf.memory_types(dmabuf),
            HandlePayload::Image(ref img) => img.memory_types(dmabuf),
            _ => Vec::new(),
        };

        get_memory_flags(memory_types)
    }

    fn bind_memory(
        &self,
        handle: &mut Handle,
        flags: MemoryFlags,
        dmabuf: Option<OwnedFd>,
    ) -> Result<()> {
        match handle.payload {
            HandlePayload::Buffer(ref mut buf) => {
                let mt_idx = find_mt(flags, buf.memory_types(dmabuf.as_ref()))?;
                buf.bind_memory(mt_idx, dmabuf)
            }
            HandlePayload::Image(ref mut img) => {
                let mt_idx = find_mt(flags, img.memory_types(dmabuf.as_ref()))?;
                img.bind_memory(mt_idx, dmabuf)
            }
            _ => Err(Error::NoSupport),
        }
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
