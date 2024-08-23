// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{Class, Constraint, Extent, Handle, HandlePayload, Layout};
use crate::dma_buf;
use crate::types::{Error, Result};
use crate::utils;
use std::os::fd::OwnedFd;

pub struct Backend {
    fd: OwnedFd,
}

impl super::Backend for Backend {
    fn allocate(&self, class: &Class, extent: Extent, con: Option<Constraint>) -> Result<Handle> {
        let layout = Layout::packed(class, extent, con)?;
        let memfd = utils::memfd_create("udmabuf", layout.size)?;
        let dmabuf = utils::udmabuf_alloc(&self.fd, memfd, layout.size)?;
        let payload = dma_buf::Payload::new(layout, dmabuf);
        let handle = Handle::new(HandlePayload::DmaBuf(payload));

        Ok(handle)
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
