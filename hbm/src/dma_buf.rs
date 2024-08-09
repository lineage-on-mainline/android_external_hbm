// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Class, Description, Extent, Flags, Handle, HandlePayload, Layout, Usage};
use super::types::{Access, Error, Mapping, Result};
use super::utils;
use std::os::fd::OwnedFd;

pub fn classify(desc: Description, usage: Usage) -> Result<Class> {
    if !desc.is_buffer() && !desc.modifier.is_linear() {
        return Err(Error::NoSupport);
    }

    let unsupported_flags = Flags::COHERENT | Flags::PROTECTED;
    if desc.flags.intersects(unsupported_flags) {
        return Err(Error::NoSupport);
    }

    let class = Class::new(desc)
        .usage(usage)
        .max_extent(Extent::max())
        .modifiers(vec![desc.modifier]);

    Ok(class)
}

pub fn import_dma_buf(
    class: &Class,
    extent: Extent,
    dmabuf: OwnedFd,
    layout: Layout,
) -> Result<Handle> {
    let packed = Layout::packed(class, extent, None)?;
    if packed.size > layout.size
        || packed.modifier != layout.modifier
        || packed.plane_count != layout.plane_count
    {
        return Err(Error::InvalidParam);
    }

    let size = utils::seek_end(&dmabuf)?;
    if size < layout.size {
        return Err(Error::InvalidParam);
    }

    let handle = Handle::with_dma_buf(dmabuf, layout);
    Ok(handle)
}

pub fn export_dma_buf(handle: &Handle, name: Option<&str>) -> Result<OwnedFd> {
    let (dmabuf, _) = match &handle.payload {
        HandlePayload::DmaBuf(v) => v,
        _ => return Err(Error::NoSupport),
    };

    if let Some(name) = name {
        let _ = utils::dma_buf_set_name(dmabuf, name);
    }

    let dmabuf = dmabuf.try_clone().map_err(Error::from)?;

    Ok(dmabuf)
}

pub fn layout(handle: &Handle) -> Result<Layout> {
    let (_, layout) = match &handle.payload {
        HandlePayload::DmaBuf(v) => v,
        _ => return Err(Error::NoSupport),
    };

    Ok(*layout)
}

pub fn map(handle: &Handle) -> Result<Mapping> {
    let (dmabuf, _) = match &handle.payload {
        HandlePayload::DmaBuf(v) => v,
        _ => return Err(Error::NoSupport),
    };

    let len = utils::seek_end(dmabuf)?;
    let mapping = utils::mmap(dmabuf, len, Access::ReadWrite)?;

    Ok(mapping)
}

pub fn unmap(_handle: &Handle, mapping: Mapping) {
    let _ = utils::munmap(mapping);
}

// TODO DMA_BUF_IOCTL_EXPORT_SYNC_FILE
// TODO DMA_BUF_IOCTL_IMPORT_SYNC_FILE
pub fn wait(handle: &Handle, access: Access) -> Result<()> {
    let (dmabuf, _) = match &handle.payload {
        HandlePayload::DmaBuf(v) => v,
        _ => return Err(Error::NoSupport),
    };

    utils::poll(dmabuf, access).and(Ok(()))
}

/// When start is true, this transfers ownership to cpu.  It implies wait and cache invalidate.
/// When start is false, this transfers ownership to device.  It implies cache flush.
pub fn sync(handle: &Handle, access: Access, start: bool) -> Result<()> {
    let (dmabuf, _) = match &handle.payload {
        HandlePayload::DmaBuf(v) => v,
        _ => return Err(Error::NoSupport),
    };

    utils::dma_buf_sync(dmabuf, access, start)
}
