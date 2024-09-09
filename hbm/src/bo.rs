// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

//! BO-related types.
//!
//! This module defines `Bo`.

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

/// A buffer object (BO).
///
/// A BO is an abstraction of a hardware buffer object.
pub struct Bo {
    device: Arc<Device>,
    handle: Handle,

    flags: Flags,
    format: Format,
    backend_index: usize,
    extent: Extent,

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
        con.modifiers
            .retain(|m1| class.modifiers.iter().any(|m2| m1 == m2));
        if con.modifiers.is_empty() {
            return Error::unsupported();
        }
    }

    Ok(Some(con))
}

impl Bo {
    fn new(device: Arc<Device>, handle: Handle, class: &Class, extent: Extent) -> Self {
        let state = BoState {
            bound: false,
            mt: MemoryType::empty(),
            mapping: None,
            map_count: 0,
        };

        Self {
            device,
            handle,
            flags: class.flags,
            format: class.format,
            backend_index: class.backend_index,
            extent,
            state: Mutex::new(state),
        }
    }

    /// Creates a BO with an optional constraint.
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
        let bo = Self::new(device, handle, class, extent);

        Ok(bo)
    }

    /// Creates a BO with an explicit physical layout.
    ///
    /// When importing, `dmabuf` can be specified to further restrict the supported memory types.
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
        let bo = Self::new(device, handle, class, extent);

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

    /// Returns the physical layout.
    pub fn layout(&self) -> Layout {
        self.backend().layout(&self.handle)
    }

    /// Returns the supported memory types.
    ///
    /// When not importing, the supported memory types can be pre-determined to some degree.  If
    /// two BOs have the same `format`, `modifier`, `flags`, and `usage`, they have the same
    /// supported memory types.
    ///
    /// If two BOs have the same `format` and `modifier`, and if the first BO's `flags` and `usage`
    /// is a subset of the second's, then the first BO's supported memory types is a superset of
    /// the second's.
    ///
    /// When importing, the supported memory types are further restricted by the imported dma-bufs.
    pub fn memory_types(&self) -> Vec<MemoryType> {
        self.backend().memory_types(&self.handle)
    }

    /// Allocates or imports a memory, and binds the memory to a BO.
    ///
    /// A BO without a memory bound cannot be exported, mapped, nor copied.
    ///
    /// As a note, two HBM BOs can refer to the same kernel space BO due to export/import.
    pub fn bind_memory(&mut self, mt: MemoryType, dmabuf: Option<OwnedFd>) -> Result<()> {
        if dmabuf.is_some() && !self.can_external() {
            return Error::user();
        }

        let mut state = self.state.lock().unwrap();
        if state.bound {
            return Error::user();
        }

        let backend = self.device.backend(self.backend_index);
        backend.bind_memory(&mut self.handle, mt, dmabuf)?;

        state.bound = true;
        state.mt = mt;

        Ok(())
    }

    /// Exports a BO as a dma-buf.
    ///
    /// A name can optionally be set for the dma-buf.
    ///
    /// As a note, two userspace dma-buf fds can refer to the same kernel space dma-buf object.
    /// The name is attached to the kernel space dma-buf object, not the userspace dma-buf fds.
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

    /// Maps a BO for CPU access.
    ///
    /// Recursive mapping is allowed and returns the same mapping.
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

    /// Unmaps a BO.
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

    /// Flushes the CPU cache for the BO mapping.
    ///
    /// If the memory type is coherent, the CPU cache is not flushed.
    pub fn flush(&self) {
        let state = self.state.lock().unwrap();

        if state.map_count > 0 && !state.mt.contains(MemoryType::COHERENT) {
            self.backend().flush(&self.handle);
        }
    }

    /// Invalidates the CPU cache for the BO mapping.
    ///
    /// If the memory type is coherent, the CPU cache is not invalidated.
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

    fn wait_copy(&self, sync_fd: Option<OwnedFd>, wait: bool) -> Option<OwnedFd> {
        if wait {
            sync_fd.and_then(|sync_fd| {
                let _ = utils::poll(sync_fd, Access::Read);
                None
            })
        } else {
            sync_fd
        }
    }

    /// Copies between two BOs that are both buffers.
    ///
    /// `sync_fd` is an optional sync file that the copy operation waits for.
    ///
    /// If `wait` is true, this function never returns any sync file.  Otherwise, it may
    /// return a sync file associated with the copy operation.
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

        self.backend()
            .copy_buffer(&self.handle, &src.handle, copy, sync_fd)
            .map(|sync_fd| self.wait_copy(sync_fd, wait))
    }

    /// Copies between two BOs where one is a buffer and one is an image.
    ///
    /// `sync_fd` is an optional sync file that the copy operation waits for.
    ///
    /// If `wait` is true, this function never returns any sync file.  Otherwise, it may
    /// return a sync file associated with the copy operation.
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

        self.backend()
            .copy_buffer_image(&self.handle, &src.handle, copy, sync_fd)
            .map(|sync_fd| self.wait_copy(sync_fd, wait))
    }
}

impl Drop for Bo {
    fn drop(&mut self) {
        self.unmap();
        self.backend().free(&self.handle);
    }
}
