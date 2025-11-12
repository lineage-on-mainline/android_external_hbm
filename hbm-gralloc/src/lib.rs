// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

#[cfg(target_os = "android")]
mod mapper;

#[cfg(target_os = "android")]
pub use mapper::ANDROID_HAL_MAPPER_VERSION;

#[cfg(target_os = "android")]
pub use mapper::AIMapper_loadIMapper;
