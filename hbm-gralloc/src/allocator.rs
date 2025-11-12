// Copyright 2024 Google LLC
// Copyright 2025 The LineageOS Project
// SPDX-License-Identifier: MIT

use android_hardware_graphics_allocator::aidl::android::hardware::graphics::allocator::{
    AllocationError::AllocationError,
    AllocationResult::AllocationResult,
    BufferDescriptorInfo::BufferDescriptorInfo,
    IAllocator::BnAllocator,
    IAllocator::IAllocator,
};
use binder::{BinderFeatures, ExceptionCode, Interface, Result, Status, Strong};
use log::{LevelFilter, info};

const LOG_TAG: &str = "graphics_allocator_service_hbm";

pub fn main() {
    let logger_success = logger::init(
        logger::Config::default().with_tag_on_device(LOG_TAG).with_max_level(LevelFilter::Trace),
    );
    if !logger_success {
        panic!("{LOG_TAG}: Failed to start logger.");
    }

    binder::ProcessState::set_thread_pool_max_thread_count(0);

    let allocator_service = AllocatorService::default();
    let allocator_service_binder = BnAllocator::new_binder(allocator_service, BinderFeatures::default());

    let service_name = format!("{}/default", AllocatorService::get_descriptor());
    binder::add_service(&service_name, allocator_service_binder.as_binder())
        .expect("Failed to register service");

    binder::ProcessState::join_thread_pool()
}

pub struct AllocatorService {
    // Add any necessary fields here
}

impl Interface for AllocatorService {}

impl AllocatorService {
    fn new() -> Self {
        Self {
            // Initialize fields here
        }
    }
}

impl Default for AllocatorService {
    fn default() -> Self {
        Self::new()
    }
}

impl IAllocator for AllocatorService {
    fn allocate(&self, descriptor: &[u8], count: i32) -> Result<AllocationResult> {
        info!("Allocator allocate called with count={}", count);
        Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
    }

    fn allocate2(&self, descriptor: &BufferDescriptorInfo, count: i32) -> Result<AllocationResult> {
        info!("Allocator allocate2 called with count={}", count);
        Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
    }

    fn isSupported(&self, descriptor: &BufferDescriptorInfo) -> Result<bool> {
        info!("Allocator isSupported called");
        Err(Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None))
    }

    fn getIMapperLibrarySuffix(&self) -> Result<String> {
        Ok(String::from("hbm"))
    }
}
