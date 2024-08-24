// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{
    Backend, Class, Constraint, CopyBuffer, CopyBufferImage, Extent, Handle, Layout, MemoryFlags,
    ResourceFlags,
};
use super::device::Device;
use super::types::{Access, Error, Mapping, Result};
use super::utils;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard};

struct BoState {
    bound: bool,

    mapping: Option<Mapping>,
    refcount: u32,
}

pub struct Bo {
    device: Arc<Device>,
    handle: Handle,
    is_buffer: bool,
    mappable: bool,
    copyable: bool,

    state: Mutex<BoState>,
}

impl Bo {
    fn new(device: Arc<Device>, class: &Class, mut handle: Handle) -> Self {
        let is_buffer = class.description.is_buffer();
        let mappable = class.description.flags.contains(ResourceFlags::MAP);
        let copyable = class.description.flags.contains(ResourceFlags::COPY);

        handle.backend_index = class.backend_index;

        let state = BoState {
            bound: false,
            mapping: None,
            refcount: 0,
        };

        Self {
            device,
            handle,
            is_buffer,
            mappable,
            copyable,
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
            return Err(Error::InvalidParam);
        }

        let con = if con.is_some() && class.constraint.is_some() {
            con.map(|mut c| {
                c.merge(class.constraint.clone().unwrap());
                c
            })
        } else {
            con.or(class.constraint.clone())
        };

        let backend = device.backend(class.backend_index);
        let handle = backend.with_constraint(class, extent, con)?;
        let bo = Self::new(device, class, handle);

        Ok(bo)
    }

    pub fn with_layout(
        device: Arc<Device>,
        class: &Class,
        extent: Extent,
        layout: Layout,
    ) -> Result<Self> {
        if !class.validate(extent) {
            return Err(Error::InvalidParam);
        }

        let backend = device.backend(class.backend_index);
        let handle = backend.with_layout(class, extent, layout)?;
        let bo = Self::new(device, class, handle);

        Ok(bo)
    }

    fn backend(&self) -> &dyn Backend {
        self.device.backend(self.handle.backend_index)
    }

    pub fn layout(&self) -> Result<Layout> {
        self.backend().layout(&self.handle)
    }

    pub fn memory_types(&self, dmabuf: Option<BorrowedFd>) -> Vec<MemoryFlags> {
        self.backend().memory_types(&self.handle, dmabuf)
    }

    pub fn bind_memory(&mut self, flags: MemoryFlags, dmabuf: Option<OwnedFd>) -> Result<()> {
        let mut state = self.state.lock().unwrap();

        if state.bound {
            return Err(Error::InvalidParam);
        }

        let backend = self.device.backend(self.handle.backend_index);
        backend.bind_memory(&mut self.handle, flags, dmabuf)?;

        state.bound = true;

        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<BoState>> {
        let state = self.state.lock().unwrap();

        if !state.bound {
            return Err(Error::InvalidParam);
        }

        Ok(state)
    }

    pub fn export_dma_buf(&self, name: Option<&str>) -> Result<OwnedFd> {
        let _state = self.lock_state()?;

        self.backend().export_dma_buf(&self.handle, name)
    }

    pub fn map(&mut self) -> Result<Mapping> {
        if !self.mappable {
            return Err(Error::InvalidParam);
        }

        let mut state = self.lock_state()?;

        if state.refcount > 0 {
            state.refcount += 1;
            return Ok(state.mapping.unwrap());
        }

        let mapping = self.backend().map(&self.handle)?;
        state.mapping = Some(mapping);
        state.refcount = 1;

        Ok(mapping)
    }

    pub fn unmap(&mut self) {
        let mut state = self.lock_state().unwrap();

        if state.refcount == 0 {
            return;
        }

        state.refcount -= 1;
        if state.refcount == 0 {
            let mapping = state.mapping.take().unwrap();
            self.backend().unmap(&self.handle, mapping);
        }
    }

    pub fn flush(&self) -> Result<()> {
        let state = self.lock_state()?;

        if state.refcount == 0 {
            return Err(Error::InvalidParam);
        }

        self.backend().flush(&self.handle)
    }

    pub fn invalidate(&self) -> Result<()> {
        let state = self.lock_state()?;

        if state.refcount == 0 {
            return Err(Error::InvalidParam);
        }

        self.backend().invalidate(&self.handle)
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
        // TODO validate copy
        if !self.copyable || !self.is_buffer || !src.is_buffer {
            return Err(Error::InvalidParam);
        }

        let _state = self.lock_state()?;

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
        // TODO validate copy
        if self.copyable || self.is_buffer == src.is_buffer {
            return Err(Error::InvalidParam);
        }

        let _state = self.lock_state()?;

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
