// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{Handle, MemoryFlags, MemoryPriority};
use crate::dma_buf;
use crate::types::{Error, Result};
use crate::utils;
use std::os::fd::OwnedFd;

pub struct Backend {
    fd: OwnedFd,
}

impl super::Backend for Backend {
    fn bind_memory(
        &self,
        handle: &mut Handle,
        flags: MemoryFlags,
        priority: MemoryPriority,
        dmabuf: Option<OwnedFd>,
    ) -> Result<()> {
        let alloc = |size| {
            let memfd = utils::memfd_create("udmabuf", size)?;
            utils::udmabuf_alloc(&self.fd, memfd, size)
        };
        dma_buf::bind_memory(handle, flags, priority, dmabuf, alloc)
    }
}

#[derive(Default)]
pub struct Builder;

impl Builder {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn build(self) -> Result<Backend> {
        if !utils::udmabuf_exists() {
            return Err(Error::NoSupport);
        }

        let fd = utils::udmabuf_open()?;
        Ok(Backend { fd })
    }
}
