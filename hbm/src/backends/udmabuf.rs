// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

//! A backend for udmabuf.
//!
//! This module provides a backend for udmabuf.

use super::{Handle, MemoryType};
use crate::dma_buf;
use crate::types::{Error, Result};
use crate::utils;
use std::os::fd::OwnedFd;

/// A udmabuf backend.
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
        let alloc = |size| {
            let memfd = utils::memfd_create("udmabuf", size)?;
            utils::udmabuf_alloc(&self.fd, memfd, size)
        };
        dma_buf::bind_memory(handle, mt, dmabuf, alloc)
    }
}

/// A udmabuf backend builder.
#[derive(Default)]
pub struct Builder;

impl Builder {
    /// Creates a udmabuf backend builder.
    pub fn new() -> Self {
        Default::default()
    }

    /// Builds a udmabuf backend.
    pub fn build(self) -> Result<Backend> {
        if !utils::udmabuf_exists() {
            return Error::unsupported();
        }

        let fd = utils::udmabuf_open()?;
        Ok(Backend { fd })
    }
}
