// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{
    Class, Constraint, CopyBuffer, CopyBufferImage, Description, Extent, Flags, Handle,
    HandlePayload, Layout, MemoryType,
};
use crate::formats;
use crate::sash;
use crate::types::{Access, Error, Format, Mapping, Modifier, Result};
use crate::utils;
use ash::vk;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::{num, ptr};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct Usage: u32 {
        const TRANSFER = 1 << 0;
        const UNIFORM = 1 << 1;
        const STORAGE = 1 << 2;
        const SAMPLED = 1 << 3;
        const COLOR = 1 << 4;
        // TODO remove this in favor of modifiers and constraints
        const SCANOUT_HACK = 1 << 5;
    }
}

fn get_usage(usage: super::Usage, valid_usage: Usage) -> Result<Usage> {
    let usage = match usage {
        super::Usage::Vulkan(usage) => usage,
        _ => return Error::user(),
    };

    if !valid_usage.contains(usage) {
        return Error::user();
    }

    Ok(usage)
}

fn get_buffer_info(flags: Flags, usage: super::Usage) -> Result<sash::BufferInfo> {
    let valid_usage = Usage::TRANSFER | Usage::UNIFORM | Usage::STORAGE;
    let usage = get_usage(usage, valid_usage)?;

    let mut buf_flags = vk::BufferCreateFlags::empty();
    let mut buf_usage = vk::BufferUsageFlags::empty();

    if flags.contains(Flags::PROTECTED) {
        buf_flags |= vk::BufferCreateFlags::PROTECTED;
    }

    if flags.contains(Flags::COPY) || usage.contains(Usage::TRANSFER) {
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
        external: flags.contains(Flags::EXTERNAL),
    };

    Ok(buf_info)
}

fn get_image_info(flags: Flags, fmt: Format, usage: super::Usage) -> Result<sash::ImageInfo> {
    let valid_usage =
        Usage::TRANSFER | Usage::STORAGE | Usage::SAMPLED | Usage::COLOR | Usage::SCANOUT_HACK;
    let usage = get_usage(usage, valid_usage)?;

    let mut img_flags = vk::ImageCreateFlags::empty();
    let mut img_usage = vk::ImageUsageFlags::empty();
    let (img_fmt, _) = formats::to_vk(fmt)?;

    if flags.contains(Flags::PROTECTED) {
        img_flags |= vk::ImageCreateFlags::PROTECTED;
    }

    if flags.contains(Flags::COPY) || usage.contains(Usage::TRANSFER) {
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
        external: flags.contains(Flags::EXTERNAL),
        no_compression: flags.contains(Flags::NO_COMPRESSION),
        scanout_hack: usage.contains(Usage::SCANOUT_HACK),
    };

    Ok(img_info)
}

fn mt_flags_to_mt(mt_flags: vk::MemoryPropertyFlags) -> MemoryType {
    let mut mt = MemoryType::empty();
    if mt_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
        mt |= MemoryType::LOCAL;
    }
    if mt_flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
        mt |= MemoryType::MAPPABLE;
        if mt_flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT) {
            mt |= MemoryType::COHERENT;
        }
        if mt_flags.contains(vk::MemoryPropertyFlags::HOST_CACHED) {
            mt |= MemoryType::CACHED;
        }
    }

    mt
}

fn mt_flags_from_mt(mt: MemoryType) -> vk::MemoryPropertyFlags {
    let mut mt_flags = vk::MemoryPropertyFlags::empty();
    if mt.contains(MemoryType::LOCAL) {
        mt_flags |= vk::MemoryPropertyFlags::DEVICE_LOCAL;
    }
    if mt.contains(MemoryType::MAPPABLE) {
        mt_flags |= vk::MemoryPropertyFlags::HOST_VISIBLE;
        if mt.contains(MemoryType::COHERENT) {
            mt_flags |= vk::MemoryPropertyFlags::HOST_COHERENT;
        }
        if mt.contains(MemoryType::CACHED) {
            mt_flags |= vk::MemoryPropertyFlags::HOST_CACHED;
        }
    }

    mt_flags
}

fn best_mt_index(mts: Vec<(u32, vk::MemoryPropertyFlags)>, mt: MemoryType) -> Result<u32> {
    let required_flags = mt_flags_from_mt(mt);
    let mut mt_iter = mts
        .into_iter()
        .filter(|(_, mt_flags)| mt_flags.contains(required_flags));

    let first_mt = mt_iter.next();
    if first_mt.is_none() {
        return Error::user();
    }

    let known_mt_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL
        | vk::MemoryPropertyFlags::HOST_VISIBLE
        | vk::MemoryPropertyFlags::HOST_COHERENT
        | vk::MemoryPropertyFlags::HOST_CACHED;
    let best_mt = mt_iter.find(|(_, mt_flags)| (*mt_flags & known_mt_flags) == required_flags);

    let mt_idx = best_mt.or(first_mt).unwrap().0;

    Ok(mt_idx)
}

fn get_memory(handle: &Handle) -> (&sash::Memory, vk::DeviceSize) {
    match &handle.payload {
        HandlePayload::Buffer(buf) => (buf.memory(), buf.size()),
        HandlePayload::Image(img) => (img.memory(), img.size()),
        _ => unreachable!(),
    }
}

fn get_buffer(handle: &Handle) -> &sash::Buffer {
    match &handle.payload {
        HandlePayload::Buffer(buf) => buf,
        _ => unreachable!(),
    }
}

fn get_image(handle: &Handle) -> &sash::Image {
    match &handle.payload {
        HandlePayload::Image(img) => img,
        _ => unreachable!(),
    }
}

pub struct Backend {
    device: Arc<sash::Device>,
}

impl Backend {
    fn new(device_index: Option<usize>, device_id: Option<u64>, debug: bool) -> Result<Self> {
        let backend = Self {
            device: sash::Device::build("hbm", device_index, device_id, debug)?,
        };

        log::info!("vulkan backend initialized");

        Ok(backend)
    }
}

impl super::Backend for Backend {
    fn memory_plane_count(&self, fmt: Format, modifier: Modifier) -> Result<u32> {
        let (fmt, _) = formats::to_vk(fmt)?;
        self.device.memory_plane_count(fmt, modifier)
    }

    fn classify(&self, desc: Description, usage: super::Usage) -> Result<Class> {
        let class = if desc.is_buffer() {
            let buf_info = get_buffer_info(desc.flags, usage)?;
            let buf_props = self.device.buffer_properties(buf_info)?;

            Class::new(desc)
                .usage(usage)
                .max_extent(Extent::Buffer(buf_props.max_size))
                .unknown_constraint()
        } else {
            let img_info = get_image_info(desc.flags, desc.format, usage)?;
            let img_props = self.device.image_properties(img_info, desc.modifier)?;

            Class::new(desc)
                .usage(usage)
                .max_extent(Extent::Image(img_props.max_extent, img_props.max_extent))
                .modifiers(img_props.modifiers)
                .unknown_constraint()
        };

        Ok(class)
    }

    fn with_constraint(
        &self,
        class: &Class,
        extent: Extent,
        con: Option<Constraint>,
    ) -> Result<Handle> {
        let handle = if class.is_buffer() {
            let buf_info = get_buffer_info(class.flags, class.usage)?;
            let buf =
                sash::Buffer::with_constraint(self.device.clone(), buf_info, extent.size(), con)?;

            Handle::new(HandlePayload::Buffer(buf))
        } else {
            let img_info = get_image_info(class.flags, class.format, class.usage)?;

            let img = sash::Image::with_constraint(
                self.device.clone(),
                img_info,
                extent.width(),
                extent.height(),
                &class.modifiers,
                con,
            )?;

            Handle::new(HandlePayload::Image(img))
        };

        Ok(handle)
    }

    fn with_layout(
        &self,
        class: &Class,
        extent: Extent,
        layout: Layout,
        dmabuf: Option<BorrowedFd>,
    ) -> Result<Handle> {
        let handle = if class.is_buffer() {
            let buf_info = get_buffer_info(class.flags, class.usage)?;
            let buf = sash::Buffer::with_layout(
                self.device.clone(),
                buf_info,
                extent.size(),
                layout,
                dmabuf,
            )?;

            Handle::new(HandlePayload::Buffer(buf))
        } else {
            let img_info = get_image_info(class.flags, class.format, class.usage)?;
            let img = sash::Image::with_layout(
                self.device.clone(),
                img_info,
                extent.width(),
                extent.height(),
                layout,
                dmabuf,
            )?;

            Handle::new(HandlePayload::Image(img))
        };

        Ok(handle)
    }

    fn layout(&self, handle: &Handle) -> Layout {
        match &handle.payload {
            HandlePayload::Buffer(buf) => buf.layout(),
            HandlePayload::Image(img) => img.layout(),
            _ => unreachable!(),
        }
    }

    fn memory_types(&self, handle: &Handle) -> Vec<MemoryType> {
        let mts = match handle.payload {
            HandlePayload::Buffer(ref buf) => buf.memory_types(),
            HandlePayload::Image(ref img) => img.memory_types(),
            _ => unreachable!(),
        };

        mts.into_iter()
            .map(|(_, mt_flags)| mt_flags_to_mt(mt_flags))
            .collect()
    }

    fn bind_memory(
        &self,
        handle: &mut Handle,
        mt: MemoryType,
        dmabuf: Option<OwnedFd>,
    ) -> Result<()> {
        match handle.payload {
            HandlePayload::Buffer(ref mut buf) => {
                let mts = buf.memory_types();
                let mt_idx = best_mt_index(mts, mt)?;
                buf.bind_memory(mt_idx, dmabuf)
            }
            HandlePayload::Image(ref mut img) => {
                let mts = img.memory_types();
                let mt_idx = best_mt_index(mts, mt)?;
                img.bind_memory(mt_idx, dmabuf)
            }
            _ => Error::unsupported(),
        }
    }

    fn export_dma_buf(&self, handle: &Handle, name: Option<&str>) -> Result<OwnedFd> {
        let (mem, _) = get_memory(handle);
        let dmabuf = mem.export_dma_buf()?;

        if let Some(name) = name {
            let _ = utils::dma_buf_set_name(&dmabuf, name);
        }

        Ok(dmabuf)
    }

    fn map(&self, handle: &Handle) -> Result<Mapping> {
        let (mem, size) = get_memory(handle);

        let len = num::NonZeroUsize::try_from(usize::try_from(size)?)?;
        let ptr = mem.map(0, size)?;
        let ptr = ptr::NonNull::new(ptr).unwrap();
        let mapping = Mapping { ptr, len };

        Ok(mapping)
    }

    fn unmap(&self, handle: &Handle, _mapping: Mapping) {
        let (mem, _) = get_memory(handle);
        mem.unmap();
    }

    fn flush(&self, handle: &Handle) {
        let (mem, size) = get_memory(handle);
        mem.flush(0, size);
    }

    fn invalidate(&self, handle: &Handle) {
        let (mem, size) = get_memory(handle);
        mem.invalidate(0, size);
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

        let dst = get_buffer(dst);
        let src = get_buffer(src);
        dst.copy_buffer(src, copy).and(Ok(None))
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
            let dst_buf = get_buffer(dst);
            let src_img = get_image(src);
            dst_buf.copy_image(src_img, copy)
        } else {
            let dst_img = get_image(dst);
            let src_buf = get_buffer(src);
            dst_img.copy_buffer(src_buf, copy)
        }
        .and(Ok(None))
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
                return Error::user();
            }
        };

        Backend::new(self.device_index, self.device_id, self.debug)
    }
}
