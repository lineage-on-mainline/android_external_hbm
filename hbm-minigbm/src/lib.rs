// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

#![warn(missing_docs)]

//! An unstable HBM C API for minigbm drivers.
//!
//! This crate provides an unstable C API for minigbm drivers.  The C API should be considered
//! internal to minigbm.  There is no plan to stabilize the API at the moment.

pub mod capi;
mod log;
