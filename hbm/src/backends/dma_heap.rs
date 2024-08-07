// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{Class, Constraint, Extent, Handle, Layout};
use crate::types::{Error, Result};
use crate::utils;
use std::os::fd::OwnedFd;

pub struct Backend {
    fd: OwnedFd,
}

impl super::Backend for Backend {
    fn allocate(&self, class: &Class, extent: Extent, con: Option<Constraint>) -> Result<Handle> {
        let layout = Layout::packed(class, extent, con)?;
        let dmabuf = utils::dma_heap_alloc(&self.fd, layout.size)?;
        let handle = Handle::with_dma_buf(dmabuf, layout);

        Ok(handle)
    }
}

#[derive(Default)]
pub struct Builder {
    heap_name: Option<String>,
    heap_fd: Option<OwnedFd>,
}

impl Builder {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn heap_name(mut self, heap_name: &str) -> Self {
        self.heap_name = Some(String::from(heap_name));
        self
    }

    pub fn heap_fd(mut self, heap_fd: OwnedFd) -> Self {
        self.heap_fd = Some(heap_fd);
        self
    }

    pub fn build(self) -> Result<Backend> {
        if self.heap_name.is_some() && self.heap_fd.is_some() {
            return Err(Error::InvalidParam);
        }

        if !utils::dma_heap_exists() {
            return Err(Error::NoSupport);
        }

        let heap_fd = if let Some(heap_name) = self.heap_name {
            utils::dma_heap_open(&heap_name)?
        } else {
            self.heap_fd.unwrap()
        };

        Ok(Backend { fd: heap_fd })
    }
}
