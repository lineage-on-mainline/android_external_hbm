// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{
    Class, Constraint, Description, Extent, Flags, Handle, HandlePayload, Layout, MemoryType, Usage,
};
use super::types::{Access, Error, Mapping, Result, Size};
use super::utils;
use std::os::fd::{BorrowedFd, OwnedFd};

pub struct Resource {
    layout: Layout,
    dmabuf: Option<OwnedFd>,
}

impl Resource {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            dmabuf: None,
        }
    }

    pub fn size(&self) -> Size {
        self.layout.size
    }

    pub fn bind_memory(&mut self, dmabuf: OwnedFd) {
        self.dmabuf = Some(dmabuf);
    }

    pub fn dmabuf(&self) -> &OwnedFd {
        self.dmabuf.as_ref().unwrap()
    }
}

fn get_resource(handle: &Handle) -> &Resource {
    match handle.payload {
        HandlePayload::DmaBuf(ref res) => res,
        _ => unreachable!(),
    }
}

fn get_resource_mut(handle: &mut Handle) -> &mut Resource {
    match handle.payload {
        HandlePayload::DmaBuf(ref mut res) => res,
        _ => unreachable!(),
    }
}

pub fn classify(desc: Description, usage: Usage) -> Result<Class> {
    if !desc.is_buffer() && !desc.modifier.is_linear() {
        return Error::unsupported();
    }

    let unsupported_flags = Flags::PROTECTED;
    if desc.flags.intersects(unsupported_flags) {
        return Error::unsupported();
    }

    let class = Class::new(desc)
        .usage(usage)
        .max_extent(Extent::max(desc.is_buffer()))
        .modifiers(vec![desc.modifier]);

    Ok(class)
}

pub fn with_constraint(class: &Class, extent: Extent, con: Option<Constraint>) -> Result<Handle> {
    let layout = Layout::packed(class, extent, con)?;
    let handle = Handle::from(Resource::new(layout));

    Ok(handle)
}

pub fn with_layout(
    class: &Class,
    extent: Extent,
    layout: Layout,
    _dmabuf: Option<BorrowedFd>,
) -> Result<Handle> {
    let packed = Layout::packed(class, extent, None)?;
    if layout.size < packed.size
        || layout.modifier != packed.modifier
        || layout.plane_count != packed.plane_count
    {
        return Error::user();
    }

    let handle = Handle::from(Resource::new(layout));

    Ok(handle)
}

pub fn layout(handle: &Handle) -> Layout {
    get_resource(handle).layout.clone()
}

pub fn memory_types(_handle: &Handle) -> Vec<MemoryType> {
    vec![MemoryType::MAPPABLE]
}

pub fn bind_memory<T>(
    handle: &mut Handle,
    mt: MemoryType,
    dmabuf: Option<OwnedFd>,
    alloc: T,
) -> Result<()>
where
    T: FnOnce(Size) -> Result<OwnedFd>,
{
    let res = get_resource_mut(handle);

    if !MemoryType::MAPPABLE.contains(mt) {
        return Error::user();
    }

    if res.dmabuf.is_some() {
        return if dmabuf.is_some() {
            Error::user()
        } else {
            Ok(())
        };
    }

    let dmabuf = if let Some(dmabuf) = dmabuf {
        let size = utils::seek_end(&dmabuf)?;
        if res.size() > size {
            return Error::user();
        }
        dmabuf
    } else {
        alloc(res.size())?
    };

    res.bind_memory(dmabuf);

    Ok(())
}

pub fn export_dma_buf(handle: &Handle, name: Option<&str>) -> Result<OwnedFd> {
    let dmabuf = get_resource(handle).dmabuf();

    if let Some(name) = name {
        let _ = utils::dma_buf_set_name(dmabuf, name);
    }

    let dmabuf = dmabuf.try_clone().map_err(Error::from)?;

    Ok(dmabuf)
}

pub fn map(handle: &Handle) -> Result<Mapping> {
    let dmabuf = get_resource(handle).dmabuf();

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

pub fn flush(handle: &Handle) {
    let dmabuf = get_resource(handle).dmabuf();

    let _ = utils::dma_buf_sync(dmabuf, Access::ReadWrite, false);
}

pub fn invalidate(handle: &Handle) {
    let dmabuf = get_resource(handle).dmabuf();

    let _ = utils::dma_buf_sync(dmabuf, Access::ReadWrite, true);
}
