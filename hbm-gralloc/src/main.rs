// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

#[cfg(target_os = "android")]
mod allocator;

#[cfg(target_os = "android")]
use allocator::main;
#[cfg(not(target_os = "android"))]
fn main() {
    println!("This service is Android-only.");
}
