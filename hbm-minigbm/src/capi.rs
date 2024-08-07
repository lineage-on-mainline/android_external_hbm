// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use libc::dev_t;
use std::collections::{hash_map::Entry, HashMap};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::{ffi, ptr, slice};

pub const HBM_FLAGS_MAPPABLE: u32 = 1 << 0;
pub const HBM_FLAGS_COHERENT: u32 = 1 << 1;
pub const HBM_FLAGS_NO_CACHE: u32 = 1 << 2;
pub const HBM_FLAGS_NO_COMPRESSION: u32 = 1 << 3;
pub const HBM_FLAGS_PROTECTED: u32 = 1 << 4;

// GPU
pub const HBM_USAGE_TRANSFER: u32 = 1 << 0;
pub const HBM_USAGE_STORAGE: u32 = 1 << 1;
pub const HBM_USAGE_SAMPLED: u32 = 1 << 2;
pub const HBM_USAGE_COLOR: u32 = 1 << 3;

#[repr(C)]
pub struct hbm_device {
    _data: [u8; 0],
}

#[repr(C)]
pub struct hbm_bo {
    _data: [u8; 0],
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct hbm_description {
    pub flags: u32,
    pub format: u32,
    pub modifier: u64,

    pub usage: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct hbm_extent_1d {
    size: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct hbm_extent_2d {
    width: u32,
    height: u32,
}

#[repr(C)]
pub union hbm_extent {
    _1d: hbm_extent_1d,
    _2d: hbm_extent_2d,
}

#[repr(C)]
pub struct hbm_constraint {
    offset_align: u64,
    stride_align: u64,
    size_align: u64,
}

#[repr(C)]
pub struct hbm_layout {
    size: u64,
    modifier: u64,
    plane_count: u32,
    offsets: [u64; 4],
    strides: [u64; 4],
}

struct HbmDevice {
    device: Arc<hbm::Device>,
    classes: Mutex<HashMap<hbm_description, hbm::Class>>,
}

fn device_from(dev: *mut hbm_device) -> Box<HbmDevice> {
    // SAFETY: dev is valid by contract
    unsafe { Box::from_raw(dev as *mut HbmDevice) }
}

fn device_into(dev: Box<HbmDevice>) -> *mut hbm_device {
    Box::into_raw(dev) as *mut hbm_device
}

fn device_as_mut<'a>(dev: *mut hbm_device) -> &'a mut HbmDevice {
    // SAFETY: dev is valid by contract
    unsafe { &mut *(dev as *mut HbmDevice) }
}

fn description_from(desc: *const hbm_description) -> hbm_description {
    // SAFETY: desc is valid by contract
    unsafe { *desc }
}

fn extent_from(extent: *const hbm_extent) -> hbm::Extent {
    // SAFETY: extent is valid by contract
    let extent = unsafe { &*extent };
    // SAFETY: we just need the raw bits
    let size = unsafe { extent._1d.size };

    hbm::Extent::new_1d(size)
}

fn constraint_from(con: *const hbm_constraint) -> Option<hbm::Constraint> {
    if con.is_null() {
        return None;
    }

    // SAFETY: con is valid by contract
    let con = unsafe { &*con };

    let con = hbm::Constraint::new()
        .offset_align(con.offset_align)
        .stride_align(con.stride_align)
        .size_align(con.size_align);

    Some(con)
}

fn bo_from(bo: *mut hbm_bo) -> Box<hbm::Bo> {
    // SAFETY: bo is valid by contract
    unsafe { Box::from_raw(bo as *mut hbm::Bo) }
}

fn bo_into(bo: Box<hbm::Bo>) -> *mut hbm_bo {
    Box::into_raw(bo) as *mut hbm_bo
}

fn bo_as_ref<'a>(bo: *mut hbm_bo) -> &'a hbm::Bo {
    // SAFETY: bo is valid by contract
    unsafe { &*(bo as *const hbm::Bo) }
}

fn bo_as_mut<'a>(bo: *mut hbm_bo) -> &'a mut hbm::Bo {
    // SAFETY: bo is valid by contract
    unsafe { &mut *(bo as *mut hbm::Bo) }
}

fn dmabuf_from(dmabuf: i32) -> OwnedFd {
    // SAFETY: dmabuf is valid by contract
    unsafe { OwnedFd::from_raw_fd(dmabuf) }
}

fn dmabuf_into(dmabuf: OwnedFd) -> RawFd {
    dmabuf.into_raw_fd()
}

fn layout_from(layout: *const hbm_layout) -> hbm::Layout {
    // SAFETY: layout is valid by contract
    let layout = unsafe { &*layout };

    hbm::Layout::new()
        .size(layout.size)
        .modifier(hbm::Modifier(layout.modifier))
        .plane_count(layout.plane_count)
        .offsets(layout.offsets)
        .strides(layout.strides)
}

fn layout_as_mut<'a>(layout: *mut hbm_layout) -> &'a mut hbm_layout {
    // SAFETY: layout is valid by contract
    unsafe { &mut *layout }
}

fn str_as_ref<'a>(s: *const ffi::c_char) -> Option<&'a str> {
    if s.is_null() {
        return None;
    }

    // SAFETY: s is valid by contract
    let s = unsafe { ffi::CStr::from_ptr(s) };

    s.to_str().ok()
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_device_create(dev: dev_t) -> *mut hbm_device {
    let backend = match hbm::vulkan::Builder::new().device_id(dev).build() {
        Ok(backend) => backend,
        _ => return ptr::null_mut(),
    };
    let device = match hbm::Builder::new().add_backend(backend).build() {
        Ok(device) => device,
        _ => return ptr::null_mut(),
    };

    let dev = HbmDevice {
        device,
        classes: Mutex::new(HashMap::new()),
    };

    let dev = Box::new(dev);
    device_into(dev)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_device_destroy(dev: *mut hbm_device) {
    let _ = device_from(dev);
}

fn device_classify(dev: &hbm::Device, desc: &hbm_description) -> Result<hbm::Class, hbm::Error> {
    let mut flags = hbm::Flags::empty();
    if (desc.flags & HBM_FLAGS_MAPPABLE) > 0 {
        flags |= hbm::Flags::MAPPABLE;
    }
    if (desc.flags & HBM_FLAGS_COHERENT) > 0 {
        flags |= hbm::Flags::COHERENT;
    }
    if (desc.flags & HBM_FLAGS_NO_CACHE) > 0 {
        flags |= hbm::Flags::NO_CACHE;
    }
    if (desc.flags & HBM_FLAGS_NO_COMPRESSION) > 0 {
        flags |= hbm::Flags::NO_COMPRESSION;
    }
    if (desc.flags & HBM_FLAGS_PROTECTED) > 0 {
        flags |= hbm::Flags::PROTECTED;
    }

    let mut usage = hbm::vulkan::Usage::empty();
    if (desc.usage & HBM_USAGE_TRANSFER) > 0 {
        usage |= hbm::vulkan::Usage::TRANSFER;
    }
    if (desc.usage & HBM_USAGE_STORAGE) > 0 {
        usage |= hbm::vulkan::Usage::STORAGE;
    }
    if (desc.usage & HBM_USAGE_SAMPLED) > 0 {
        usage |= hbm::vulkan::Usage::SAMPLED;
    }
    if (desc.usage & HBM_USAGE_COLOR) > 0 {
        usage |= hbm::vulkan::Usage::COLOR;
    }

    let desc = hbm::Description::new()
        .flags(flags)
        .format(hbm::Format(desc.format))
        .modifier(hbm::Modifier(desc.modifier));
    let usage = hbm::Usage::Vulkan(usage);

    dev.classify(desc, slice::from_ref(&usage))
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_create(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    con: *const hbm_constraint,
) -> *mut hbm_bo {
    let dev = device_as_mut(dev);
    let desc = description_from(desc);
    let extent = extent_from(extent);
    let con = constraint_from(con);

    // TODO reduce lock scope and avoid copy-n-paste
    let mut classes = dev.classes.lock().unwrap();
    let class: &hbm::Class = match classes.entry(desc) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let class = match device_classify(&dev.device, e.key()) {
                Ok(class) => class,
                _ => return ptr::null_mut(),
            };
            e.insert(class)
        }
    };

    let bo = match hbm::Bo::new(dev.device.clone(), class, extent, con) {
        Ok(v) => v,
        _ => return ptr::null_mut(),
    };

    let bo = Box::new(bo);
    bo_into(bo)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_import_dma_buf(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    dmabuf: i32,
    layout: *const hbm_layout,
) -> *mut hbm_bo {
    let dev = device_as_mut(dev);
    let desc = description_from(desc);
    let extent = extent_from(extent);
    let dmabuf = dmabuf_from(dmabuf);
    let layout = layout_from(layout);

    // TODO reduce lock scope and avoid copy-n-paste
    let mut classes = dev.classes.lock().unwrap();
    let class: &hbm::Class = match classes.entry(desc) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => {
            let class = match device_classify(&dev.device, e.key()) {
                Ok(class) => class,
                _ => return ptr::null_mut(),
            };
            e.insert(class)
        }
    };

    let bo = match hbm::Bo::with_dma_buf(dev.device.clone(), class, extent, dmabuf, layout) {
        Ok(v) => v,
        _ => return ptr::null_mut(),
    };

    let bo = Box::new(bo);
    bo_into(bo)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_destroy(bo: *mut hbm_bo) {
    let _ = bo_from(bo);
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_export_dma_buf(
    bo: *mut hbm_bo,
    name: *const ffi::c_char,
    out_layout: *mut hbm_layout,
) -> i32 {
    let bo = bo_as_ref(bo);
    let name = str_as_ref(name);
    let out_layout = layout_as_mut(out_layout);

    let (dmabuf, layout) = match bo.export_dma_buf(name) {
        Ok(v) => v,
        _ => return -1,
    };

    out_layout.size = layout.size;
    out_layout.modifier = layout.modifier.0;
    out_layout.plane_count = layout.plane_count;
    out_layout.offsets = layout.offsets;
    out_layout.strides = layout.strides;

    dmabuf_into(dmabuf)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_map(bo: *mut hbm_bo) -> *mut ffi::c_void {
    let bo = bo_as_mut(bo);

    let mapping = match bo.map() {
        Ok(v) => v,
        _ => return ptr::null_mut(),
    };

    mapping.ptr.as_ptr()
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_unmap(bo: *mut hbm_bo) {
    let bo = bo_as_mut(bo);

    bo.unmap();
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_flush(bo: *mut hbm_bo) {
    let bo = bo_as_ref(bo);

    let _ = bo.flush();
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_invalidate(bo: *mut hbm_bo) {
    let bo = bo_as_ref(bo);

    let _ = bo.invalidate();
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_copy_buffer(
    bo: *mut hbm_bo,
    src: *mut hbm_bo,
    src_offset: u64,
    dst_offset: u64,
    size: u64,
) -> bool {
    let bo = bo_as_ref(bo);
    let src = bo_as_ref(src);

    let copy = hbm::CopyBuffer {
        src_offset,
        dst_offset,
        size,
    };
    match bo.copy_buffer(src, copy, None) {
        Ok(sync_fd) => {
            if let Some(_sync_fd) = sync_fd {
                // TODO
            }

            true
        }
        _ => false,
    }
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_copy_buffer_image(
    bo: *mut hbm_bo,
    src: *mut hbm_bo,
    offset: u64,
    stride: u64,
    plane: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> bool {
    let bo = bo_as_ref(bo);
    let src = bo_as_ref(src);

    let copy = hbm::CopyBufferImage {
        offset,
        stride,
        plane,
        x,
        y,
        width,
        height,
    };

    match bo.copy_buffer_image(src, copy, None) {
        Ok(sync_fd) => {
            if let Some(_sync_fd) = sync_fd {
                // TODO
            }

            true
        }
        _ => false,
    }
}
