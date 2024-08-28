// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{
    Backend, Class, Constraint, CopyBuffer, CopyBufferImage, Extent, Flags, Handle, Layout,
    MemoryType,
};
use super::device::Device;
use super::formats;
use super::types::{Access, Error, Format, Mapping, Result, Size};
use super::utils;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

struct BoState {
    bound: bool,
    mt: MemoryType,

    mapping: Option<Mapping>,
    map_count: u32,
}

pub struct Bo {
    device: Arc<Device>,

    flags: Flags,
    format: Format,
    backend_index: usize,
    extent: Extent,

    handle: Handle,

    state: Mutex<BoState>,
}

fn merge_class_to_constraint(con: Option<Constraint>, class: &Class) -> Result<Option<Constraint>> {
    if con.is_none() && class.constraint.is_none() {
        return Ok(None);
    }

    let mut con = con.unwrap_or_default();
    if let Some(other) = &class.constraint {
        con.merge(other.clone());
    }

    if !con.modifiers.is_empty() {
        con.modifiers = con
            .modifiers
            .iter()
            .filter(|m1| class.modifiers.iter().any(|m2| *m1 == m2))
            .copied()
            .collect();
        if con.modifiers.is_empty() {
            return Error::unsupported();
        }
    }

    Ok(Some(con))
}

impl Bo {
    fn new(device: Arc<Device>, class: &Class, extent: Extent, handle: Handle) -> Self {
        let state = BoState {
            bound: false,
            mt: MemoryType::empty(),
            mapping: None,
            map_count: 0,
        };

        Self {
            device,
            flags: class.flags,
            format: class.format,
            backend_index: class.backend_index,
            extent,
            handle,
            state: Mutex::new(state),
        }
    }

    pub fn with_constraint(
        device: Arc<Device>,
        class: &Class,
        extent: Extent,
        con: Option<Constraint>,
    ) -> Result<Self> {
        if !class.validate(extent) {
            return Error::user();
        }

        let con = merge_class_to_constraint(con, class)?;

        let backend = device.backend(class.backend_index);
        let handle = backend.with_constraint(class, extent, con)?;
        let bo = Self::new(device, class, extent, handle);

        Ok(bo)
    }

    pub fn with_layout(
        device: Arc<Device>,
        class: &Class,
        extent: Extent,
        layout: Layout,
        dmabuf: Option<BorrowedFd>,
    ) -> Result<Self> {
        if !class.validate(extent) {
            return Error::user();
        }

        let backend = device.backend(class.backend_index);
        let handle = backend.with_layout(class, extent, layout, dmabuf)?;
        let bo = Self::new(device, class, extent, handle);

        Ok(bo)
    }

    fn can_external(&self) -> bool {
        self.flags.contains(Flags::EXTERNAL)
    }

    fn can_map(&self) -> bool {
        self.flags.contains(Flags::MAP)
    }

    fn can_copy(&self) -> bool {
        self.flags.contains(Flags::COPY)
    }

    fn is_buffer(&self) -> bool {
        self.format.is_invalid()
    }

    fn backend(&self) -> &dyn Backend {
        self.device.backend(self.backend_index)
    }

    pub fn layout(&self) -> Layout {
        self.backend().layout(&self.handle)
    }

    pub fn memory_types(&self, required_mt: MemoryType, denied_mt: MemoryType) -> Vec<MemoryType> {
        self.backend()
            .memory_types(&self.handle, required_mt, denied_mt)
    }

    pub fn bind_memory(&mut self, mt: MemoryType, dmabuf: Option<OwnedFd>) -> Result<()> {
        if dmabuf.is_some() && !self.can_external() {
            return Error::user();
        }

        let mut state = self.state.lock().unwrap();

        if state.bound {
            return Error::user();
        }

        // TODO if the bo cannot be mapped nor copied, all we need is the dma-buf
        let backend = self.device.backend(self.backend_index);
        backend.bind_memory(&mut self.handle, mt, dmabuf)?;

        state.bound = true;
        state.mt = mt;

        Ok(())
    }

    pub fn export_dma_buf(&self, name: Option<&str>) -> Result<OwnedFd> {
        if !self.can_external() {
            return Error::user();
        }

        let state = self.state.lock().unwrap();
        if !state.bound {
            return Error::user();
        }

        self.backend().export_dma_buf(&self.handle, name)
    }

    pub fn map(&mut self) -> Result<Mapping> {
        if !self.can_map() {
            return Error::user();
        }

        let mut state = self.state.lock().unwrap();
        if !state.bound || !state.mt.contains(MemoryType::MAPPABLE) {
            return Error::user();
        }

        if state.map_count == 0 {
            let mapping = self.backend().map(&self.handle)?;
            state.mapping = Some(mapping);
            state.map_count = 1;
        } else {
            state.map_count += 1;
        }

        Ok(state.mapping.unwrap())
    }

    pub fn unmap(&mut self) {
        let mut state = self.state.lock().unwrap();

        match state.map_count {
            0 => (),
            1 => {
                let mapping = state.mapping.take().unwrap();
                self.backend().unmap(&self.handle, mapping);
                state.map_count = 0;
            }
            _ => state.map_count -= 1,
        }
    }

    pub fn flush(&self) {
        let state = self.state.lock().unwrap();

        if state.map_count > 0 && !state.mt.contains(MemoryType::COHERENT) {
            self.backend().flush(&self.handle);
        }
    }

    pub fn invalidate(&self) {
        let state = self.state.lock().unwrap();

        if state.map_count > 0 && !state.mt.contains(MemoryType::COHERENT) {
            self.backend().invalidate(&self.handle);
        }
    }

    // this should not be used if the mutex needs to remain locked for synchronization
    fn is_bound(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.bound
    }

    fn validate_copy(&self, src: &Bo) -> bool {
        self.can_copy() && self.is_bound() && src.can_copy() && src.is_bound()
    }

    fn validate_copy_buffer(&self, src: &Bo, copy: &CopyBuffer) -> bool {
        if !self.validate_copy(src) || !self.is_buffer() || !src.is_buffer() {
            return false;
        }

        let src_size = src.extent.size();
        let dst_size = self.extent.size();

        copy.size > 0
            && copy.src_offset <= src_size
            && copy.size <= src_size - copy.src_offset
            && copy.dst_offset <= dst_size
            && copy.size <= dst_size - copy.dst_offset
    }

    fn validate_copy_buffer_image(&self, src: &Bo, copy: &CopyBufferImage) -> bool {
        if !self.validate_copy(src) || (self.is_buffer() == src.is_buffer()) {
            return false;
        }

        let size;
        let mut width;
        let mut height;
        let fmt;
        if self.is_buffer() {
            size = self.extent.size();
            width = src.extent.width();
            height = src.extent.height();
            fmt = src.format;
        } else {
            size = src.extent.size();
            width = self.extent.width();
            height = self.extent.height();
            fmt = self.format;
        }

        let fmt_class = formats::format_class(fmt).unwrap();
        let plane_count = fmt_class.plane_count as u32;
        if copy.plane >= plane_count {
            return false;
        }

        let bpp = fmt_class.block_size[copy.plane as usize] as Size;
        width /= fmt_class.block_extent[copy.plane as usize].0 as u32;
        height /= fmt_class.block_extent[copy.plane as usize].1 as u32;

        if copy.offset % bpp != 0
            || copy.stride % bpp != 0
            || copy.stride / bpp < copy.width as Size
        {
            return false;
        }

        copy.width > 0
            && copy.height > 0
            && copy.offset <= size
            && copy.stride <= (size - copy.offset) / copy.height as Size
            && copy.x <= width
            && copy.y <= height
            && copy.width <= width - copy.x
            && copy.height <= height - copy.y
    }

    fn wait_copy(&self, sync_fd: Option<OwnedFd>, wait: bool) -> Result<Option<OwnedFd>> {
        let sync_fd = if wait {
            sync_fd.and_then(|sync_fd| {
                let _ = utils::poll(sync_fd, Access::Read);
                None
            })
        } else {
            sync_fd
        };

        Ok(sync_fd)
    }

    pub fn copy_buffer(
        &self,
        src: &Bo,
        copy: CopyBuffer,
        sync_fd: Option<OwnedFd>,
        wait: bool,
    ) -> Result<Option<OwnedFd>> {
        if !self.validate_copy_buffer(src, &copy) {
            return Error::user();
        }

        let sync_fd = self
            .backend()
            .copy_buffer(&self.handle, &src.handle, copy, sync_fd)?;

        self.wait_copy(sync_fd, wait)
    }

    pub fn copy_buffer_image(
        &self,
        src: &Bo,
        copy: CopyBufferImage,
        sync_fd: Option<OwnedFd>,
        wait: bool,
    ) -> Result<Option<OwnedFd>> {
        if !self.validate_copy_buffer_image(src, &copy) {
            return Error::user();
        }

        let sync_fd = self
            .backend()
            .copy_buffer_image(&self.handle, &src.handle, copy, sync_fd)?;

        self.wait_copy(sync_fd, wait)
    }
}

impl Drop for Bo {
    fn drop(&mut self) {
        self.unmap();
        self.backend().free(&self.handle);
    }
}
