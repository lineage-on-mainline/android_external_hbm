// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

#![warn(missing_docs)]

//! HBM is a hardware buffer allocator.
//!
//! This crate provides a library to allocate, export/import, and access hardware buffer objects
//! (BOs).
//!
//! A BO allocation is divided into 3 steps.  The classification step validates the parameters and
//! checks for hardware support.  The create step creates the BO descriptor, which encompasses the
//! physical layout and the supported memory types.  The bind step creates or imports a memory, and
//! binds the memory to the BO.

mod backends;
mod bo;
mod device;
mod dma_buf;
mod formats;
#[cfg(feature = "ash")]
mod sash;
mod types;
mod utils;

pub use backends::*;
pub use bo::*;
pub use device::*;
pub use types::*;
