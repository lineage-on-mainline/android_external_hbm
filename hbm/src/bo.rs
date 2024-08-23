// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{
    Backend, Class, Constraint, CopyBuffer, CopyBufferImage, Extent, Flags, Handle, Layout,
};
use super::device::Device;
use super::types::{Access, Error, Mapping, Result};
use super::utils;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

struct MappingState {
    refcount: u32,
    mapping: Option<Mapping>,
}

pub struct Bo {
    device: Arc<Device>,
    handle: Handle,
    is_buffer: bool,
    mappable: bool,

    // is this how mutex works?
    state: Mutex<MappingState>,
}

impl Bo {
    pub fn new(
        device: Arc<Device>,
        class: &Class,
        extent: Extent,
        con: Option<Constraint>,
    ) -> Result<Self> {
        // what if class is from another device?
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

        let backend = &device.backends[class.backend_index];

        let mut handle = backend.with_constraint(class, extent, con)?;
        backend.bind_memory(&mut handle, class, None)?;

        let bo = Self::with_handle(device, class, handle);

        Ok(bo)
    }

    pub fn with_dma_buf(
        device: Arc<Device>,
        class: &Class,
        extent: Extent,
        dmabuf: OwnedFd,
        layout: Layout,
    ) -> Result<Self> {
        if !class.validate(extent) {
            return Err(Error::InvalidParam);
        }

        let backend = &device.backends[class.backend_index];

        let mut handle = backend.with_layout(class, extent, layout)?;
        backend.bind_memory(&mut handle, class, Some(dmabuf))?;

        let bo = Self::with_handle(device, class, handle);

        Ok(bo)
    }

    fn with_handle(device: Arc<Device>, class: &Class, mut handle: Handle) -> Self {
        let is_buffer = class.description.is_buffer();
        let mappable = class.description.flags.contains(Flags::MAP);

        handle.backend_index = class.backend_index;

        Self {
            device,
            handle,
            is_buffer,
            mappable,
            state: Mutex::new(MappingState {
                refcount: 0,
                mapping: None,
            }),
        }
    }

    fn backend(&self) -> &dyn Backend {
        self.device.backend(self.handle.backend_index)
    }

    pub fn export_dma_buf(&self, name: Option<&str>) -> Result<OwnedFd> {
        // TODO this can race with map/unmap
        self.backend().export_dma_buf(&self.handle, name)
    }

    pub fn layout(&self) -> Result<Layout> {
        self.backend().layout(&self.handle)
    }

    pub fn map(&mut self) -> Result<Mapping> {
        if !self.mappable {
            return Err(Error::InvalidParam);
        }

        let mut state = self.state.lock().unwrap();

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
        let mut state = self.state.lock().unwrap();

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
        let state = self.state.lock().unwrap();

        if state.refcount == 0 {
            return Err(Error::InvalidParam);
        }

        self.backend().flush(&self.handle)
    }

    pub fn invalidate(&self) -> Result<()> {
        let state = self.state.lock().unwrap();

        if state.refcount == 0 {
            return Err(Error::InvalidParam);
        }

        self.backend().invalidate(&self.handle)
    }

    pub fn copy_buffer(
        &self,
        src: &Bo,
        copy: CopyBuffer,
        sync_fd: Option<OwnedFd>,
        sync: bool,
    ) -> Result<Option<OwnedFd>> {
        if !self.is_buffer || !src.is_buffer {
            return Err(Error::InvalidParam);
        }

        // what if src is from another device?
        let ret = self
            .backend()
            .copy_buffer(&self.handle, &src.handle, copy, sync_fd);
        if !sync {
            return ret;
        }

        ret.map(|sync_fd| {
            if let Some(sync_fd) = sync_fd {
                let _ = utils::poll(sync_fd, Access::Read);
            }
            None
        })
    }

    pub fn copy_buffer_image(
        &self,
        src: &Bo,
        copy: CopyBufferImage,
        sync_fd: Option<OwnedFd>,
        sync: bool,
    ) -> Result<Option<OwnedFd>> {
        if self.is_buffer == src.is_buffer {
            return Err(Error::InvalidParam);
        }

        // what if src is from another device?
        let ret = self
            .backend()
            .copy_buffer_image(&self.handle, &src.handle, copy, sync_fd);
        if !sync {
            return ret;
        }

        ret.map(|sync_fd| {
            if let Some(sync_fd) = sync_fd {
                let _ = utils::poll(sync_fd, Access::Read);
            }
            None
        })
    }
}

impl Drop for Bo {
    fn drop(&mut self) {
        self.unmap();
        self.backend().free(&self.handle);
    }
}
