// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Class, Description, Extent, Flags, Handle, HandlePayload, Layout, Usage};
use super::types::{Access, Error, Mapping, Result};
use super::utils;
use std::os::fd::OwnedFd;

pub struct Resource {
    pub layout: Layout,
    dmabuf: Option<OwnedFd>,
}

impl Resource {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            dmabuf: None,
        }
    }

    pub fn bind(&mut self, dmabuf: OwnedFd) {
        self.dmabuf = Some(dmabuf);
    }

    pub fn dmabuf(&self) -> &OwnedFd {
        self.dmabuf.as_ref().unwrap()
    }
}

impl From<Resource> for Handle {
    fn from(res: Resource) -> Self {
        Handle::new(HandlePayload::DmaBuf(res))
    }
}

impl AsRef<Resource> for Handle {
    fn as_ref(&self) -> &Resource {
        match self.payload {
            HandlePayload::DmaBuf(ref res) => res,
            _ => unreachable!(),
        }
    }
}

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

    let mut res = Resource::new(layout);
    res.bind(dmabuf);

    let handle = Handle::from(res);

    Ok(handle)
}

pub fn export_dma_buf(handle: &Handle, name: Option<&str>) -> Result<OwnedFd> {
    let dmabuf = handle.as_ref().dmabuf();

    if let Some(name) = name {
        let _ = utils::dma_buf_set_name(dmabuf, name);
    }

    let dmabuf = dmabuf.try_clone().map_err(Error::from)?;

    Ok(dmabuf)
}

pub fn layout(handle: &Handle) -> Result<Layout> {
    let layout = handle.as_ref().layout;

    Ok(layout)
}

pub fn map(handle: &Handle) -> Result<Mapping> {
    let dmabuf = handle.as_ref().dmabuf();

    let len = utils::seek_end(dmabuf)?;
    let mapping = utils::mmap(dmabuf, len, Access::ReadWrite)?;

    Ok(mapping)
}

pub fn unmap(_handle: &Handle, mapping: Mapping) {
    let _ = utils::munmap(mapping);
}

// utils::dma_buf_sync is supposed to be used as follows
//
//  - utils::dma_buf_sync(dmabuf, access, true)
//  - cpu access with the specified access type
//  - utils::dma_buf_sync(dmabuf, access, false)
//
// But for our purposes, we assume it works as follows
//
//  - utils::dma_buf_sync(dmabuf, access, true)
//    - waits for the implicit fences (if any)
//    - makes sure device writes are available in the cpu domain (if any)
//    - Access::Read further invalidates the cpu cache
//  - utils::dma_buf_sync(dmabuf, access, false)
//    - Access::Write flushes the cpu cache and makes sure cpu writes are available in the device
//      domain
//
// and abuse it for flush/invalidate.  These are not used in most setups anyway.

pub fn flush(handle: &Handle) -> Result<()> {
    let dmabuf = handle.as_ref().dmabuf();

    utils::dma_buf_sync(dmabuf, Access::ReadWrite, false)
}

pub fn invalidate(handle: &Handle) -> Result<()> {
    let dmabuf = handle.as_ref().dmabuf();

    utils::dma_buf_sync(dmabuf, Access::ReadWrite, true)
}
