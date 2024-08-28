// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

//! A backend for dma-heaps.
//!
//! This module provides a backend for dma-heaps.

use super::{Handle, MemoryType};
use crate::dma_buf;
use crate::types::{Error, Result};
use crate::utils;
use std::os::fd::OwnedFd;

/// A dma-heap backend.
pub struct Backend {
    fd: OwnedFd,
}

impl super::Backend for Backend {
    fn bind_memory(
        &self,
        handle: &mut Handle,
        mt: MemoryType,
        dmabuf: Option<OwnedFd>,
    ) -> Result<()> {
        let alloc = |size| utils::dma_heap_alloc(&self.fd, size);
        dma_buf::bind_memory(handle, mt, dmabuf, alloc)
    }
}

/// A dma-heap backend builder.
#[derive(Default)]
pub struct Builder {
    heap_name: Option<String>,
    heap_fd: Option<OwnedFd>,
}

impl Builder {
    /// Creates a dma-heap backend builder.
    pub fn new() -> Self {
        Default::default()
    }

    /// Sets the name of the dma-heap to use.
    pub fn heap_name(mut self, heap_name: &str) -> Self {
        self.heap_name = Some(String::from(heap_name));
        self
    }

    /// Sets the fd of the dma-heap to use.
    pub fn heap_fd(mut self, heap_fd: OwnedFd) -> Self {
        self.heap_fd = Some(heap_fd);
        self
    }

    /// Builds a dma-heap backend.
    ///
    /// One and only one of the heap name or the heap fd must be set.
    pub fn build(self) -> Result<Backend> {
        if self.heap_name.is_some() && self.heap_fd.is_some() {
            return Error::user();
        }

        if !utils::dma_heap_exists() {
            return Error::unsupported();
        }

        let heap_fd = if let Some(heap_name) = self.heap_name {
            utils::dma_heap_open(&heap_name)?
        } else {
            self.heap_fd.unwrap()
        };

        Ok(Backend { fd: heap_fd })
    }
}
