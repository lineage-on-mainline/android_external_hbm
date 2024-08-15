// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use libc::dev_t;
use std::collections::{hash_map::Entry, HashMap};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::{ffi, ptr, slice};

pub const HBM_FLAG_MAPPABLE: u32 = 1 << 0;
pub const HBM_FLAG_COHERENT: u32 = 1 << 1;
pub const HBM_FLAG_NO_CACHE: u32 = 1 << 2;
pub const HBM_FLAG_NO_COMPRESSION: u32 = 1 << 3;
pub const HBM_FLAG_PROTECTED: u32 = 1 << 4;

// GPU
pub const HBM_USAGE_TRANSFER: u64 = 1u64 << 0;
pub const HBM_USAGE_STORAGE: u64 = 1u64 << 1;
pub const HBM_USAGE_SAMPLED: u64 = 1u64 << 2;
pub const HBM_USAGE_COLOR: u64 = 1u64 << 3;

#[repr(C)]
pub enum hbm_log_level {
    Off,
    Error,
    Warn,
    Info,
    Debug,
}

#[allow(non_camel_case_types)]
pub type hbm_log_callback = Option<
    unsafe extern "C" fn(lv: hbm_log_level, msg: *const ffi::c_char, cb_data: *mut ffi::c_void),
>;

#[repr(C)]
pub struct hbm_device {
    _data: [u8; 0],
}

type ClassCache = HashMap<hbm_description, hbm::Class>;

struct CDevice {
    device: Arc<hbm::Device>,
    class_cache: Mutex<ClassCache>,
}

impl CDevice {
    fn into(dev: Self) -> *mut hbm_device {
        let dev = Box::new(dev);
        Box::into_raw(dev) as *mut hbm_device
    }

    fn from(dev: *mut hbm_device) -> Box<Self> {
        // SAFETY: dev was created by Self::into
        unsafe { Box::from_raw(dev as *mut Self) }
    }

    fn as_mut<'a>(dev: *mut hbm_device) -> &'a mut Self {
        // SAFETY: dev was created by Self::into
        unsafe { &mut *(dev as *mut CDevice) }
    }

    fn classify(&self, desc: &hbm_description) -> Result<hbm::Class, hbm::Error> {
        let mut flags = hbm::Flags::empty();
        if (desc.flags & HBM_FLAG_MAPPABLE) > 0 {
            flags |= hbm::Flags::MAP;
        }
        if (desc.flags & HBM_FLAG_COHERENT) > 0 {
            flags |= hbm::Flags::COHERENT;
        }
        if (desc.flags & HBM_FLAG_NO_CACHE) > 0 {
            flags |= hbm::Flags::NO_CACHE;
        }
        if (desc.flags & HBM_FLAG_NO_COMPRESSION) > 0 {
            flags |= hbm::Flags::NO_COMPRESSION;
        }
        if (desc.flags & HBM_FLAG_PROTECTED) > 0 {
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

        self.device.classify(desc, slice::from_ref(&usage))
    }

    fn get_class<'a>(
        &self,
        class_cache: &'a mut ClassCache,
        desc: hbm_description,
    ) -> Result<&'a hbm::Class, hbm::Error> {
        let class: &hbm::Class = match class_cache.entry(desc) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let class = self.classify(e.key())?;
                e.insert(class)
            }
        };

        Ok(class)
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct hbm_description {
    pub flags: u32,
    pub format: u32,
    pub modifier: u64,
    pub usage: u64,
}

impl hbm_description {
    fn from(desc: *const Self) -> Self {
        // SAFETY: desc is non-NULL
        unsafe { *desc }
    }
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

impl hbm_extent {
    fn into(extent: *const hbm_extent) -> hbm::Extent {
        // SAFETY: extent is non-NULL
        let extent = unsafe { &*extent };
        // SAFETY: we just need the raw bits
        let size = unsafe { extent._1d.size };

        hbm::Extent::new_1d(size)
    }
}

#[repr(C)]
pub struct hbm_constraint {
    offset_align: u64,
    stride_align: u64,
    size_align: u64,

    modifiers: *const u64,
    modifier_count: u32,
}

impl hbm_constraint {
    fn into(con: *const hbm_constraint) -> Option<hbm::Constraint> {
        if con.is_null() {
            return None;
        }

        // SAFETY: con is non-NULL
        let con = unsafe { &*con };
        // SAFETY: con.modifiers has the right size
        let mods = unsafe { slice::from_raw_parts(con.modifiers, con.modifier_count as usize) };

        let mut con = hbm::Constraint::new()
            .offset_align(con.offset_align)
            .stride_align(con.stride_align)
            .size_align(con.size_align);
        if !mods.is_empty() {
            let mods: Vec<hbm::Modifier> = mods.iter().copied().map(hbm::Modifier::from).collect();
            con = con.modifiers(mods);
        }

        Some(con)
    }
}

#[repr(C)]
pub struct hbm_bo {
    _data: [u8; 0],
}

impl hbm_bo {
    fn from(bo: hbm::Bo) -> *mut hbm_bo {
        let bo = Box::new(bo);
        Box::into_raw(bo) as *mut hbm_bo
    }

    fn into(bo: *mut Self) -> Box<hbm::Bo> {
        // SAFETY: bo was created by Self::from
        unsafe { Box::from_raw(bo as *mut hbm::Bo) }
    }

    fn as_ref<'a>(bo: *mut Self) -> &'a hbm::Bo {
        // SAFETY: bo was created by Self::from
        unsafe { &*(bo as *const hbm::Bo) }
    }

    fn as_mut<'a>(bo: *mut Self) -> &'a mut hbm::Bo {
        // SAFETY: bo was created by Self::from
        unsafe { &mut *(bo as *mut hbm::Bo) }
    }
}

fn dmabuf_from(dmabuf: i32) -> OwnedFd {
    // SAFETY: dmabuf is valid by contract
    unsafe { OwnedFd::from_raw_fd(dmabuf) }
}

fn dmabuf_into(dmabuf: OwnedFd) -> RawFd {
    dmabuf.into_raw_fd()
}

#[repr(C)]
pub struct hbm_layout {
    size: u64,
    modifier: u64,
    plane_count: u32,
    offsets: [u64; 4],
    strides: [u64; 4],
}

impl hbm_layout {
    fn into(layout: *const Self) -> hbm::Layout {
        // SAFETY: layout is non-NULL
        let layout = unsafe { &*layout };

        hbm::Layout::new()
            .size(layout.size)
            .modifier(hbm::Modifier(layout.modifier))
            .plane_count(layout.plane_count)
            .offsets(layout.offsets)
            .strides(layout.strides)
    }

    fn as_mut<'a>(layout: *mut hbm_layout) -> &'a mut hbm_layout {
        // SAFETY: layout is non_NULL
        unsafe { &mut *layout }
    }
}

fn str_as_ref<'a>(s: *const ffi::c_char) -> Option<&'a str> {
    if s.is_null() {
        return None;
    }

    // SAFETY: s is a non-NULL and nul-terminated
    let s = unsafe { ffi::CStr::from_ptr(s) };

    s.to_str().ok()
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_log_init(
    max_lv: hbm_log_level,
    log_cb: hbm_log_callback,
    cb_data: *mut ffi::c_void,
) {
    let filter = match max_lv {
        hbm_log_level::Off => log::LevelFilter::Off,
        hbm_log_level::Error => log::LevelFilter::Error,
        hbm_log_level::Warn => log::LevelFilter::Warn,
        hbm_log_level::Info => log::LevelFilter::Info,
        hbm_log_level::Debug => log::LevelFilter::Debug,
    };

    if filter == log::LevelFilter::Off || log_cb.is_none() {
        super::log::init(log::LevelFilter::Off, Box::new(|_| {}));
        return;
    }

    let log_cb = log_cb.unwrap();
    let cb_data = cb_data as usize;
    let cb = move |rec: &log::Record| {
        let lv = match rec.level() {
            log::Level::Error => hbm_log_level::Error,
            log::Level::Warn => hbm_log_level::Warn,
            log::Level::Info => hbm_log_level::Info,
            log::Level::Debug => hbm_log_level::Debug,
            log::Level::Trace => hbm_log_level::Debug,
        };
        let msg = format!("{}", rec.args());
        if let Ok(c_msg) = ffi::CString::new(msg) {
            // SAFETY: we trust the client
            unsafe {
                log_cb(lv, c_msg.as_ptr(), cb_data as *mut ffi::c_void);
            }
        }
    };

    super::log::init(filter, Box::new(cb));
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_device_create(dev: dev_t) -> *mut hbm_device {
    let backend = match hbm::vulkan::Builder::new().device_id(dev).build() {
        Ok(backend) => backend,
        _ => return ptr::null_mut(),
    };
    let device = match hbm::Builder::new().add_backend(backend).build() {
        Ok(dev) => dev,
        _ => return ptr::null_mut(),
    };

    let dev = CDevice {
        device,
        class_cache: Mutex::new(HashMap::new()),
    };

    CDevice::into(dev)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_device_destroy(dev: *mut hbm_device) {
    let _ = CDevice::from(dev);
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_device_get_plane_count(
    dev: *mut hbm_device,
    fmt: u32,
    modifier: u64,
) -> u32 {
    let dev = CDevice::as_mut(dev);

    match dev
        .device
        .plane_count(hbm::Format(fmt), hbm::Modifier(modifier))
    {
        Ok(count) => count,
        _ => 0,
    }
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_device_get_modifiers(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    out_mods: *mut u64,
) -> i32 {
    let dev = CDevice::as_mut(dev);
    let desc = hbm_description::from(desc);

    // TODO is it possible to reduce lock scope?
    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return -1,
    };

    let mods = match dev.device.modifiers(class) {
        Some(mods) => mods,
        None => return 0,
    };

    if !out_mods.is_null() {
        // SAFETY: out_mods must be large enough for mods.len() modifiers
        let out_mods = unsafe { slice::from_raw_parts_mut(out_mods, mods.len()) };

        for (dst, src) in out_mods.iter_mut().zip(mods.iter()) {
            *dst = src.0;
        }
    }

    mods.len() as i32
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_create(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    con: *const hbm_constraint,
) -> *mut hbm_bo {
    let dev = CDevice::as_mut(dev);
    let desc = hbm_description::from(desc);
    let extent = hbm_extent::into(extent);
    let con = hbm_constraint::into(con);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return ptr::null_mut(),
    };

    let bo = match hbm::Bo::new(dev.device.clone(), class, extent, con) {
        Ok(bo) => bo,
        _ => return ptr::null_mut(),
    };

    hbm_bo::from(bo)
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
    let dev = CDevice::as_mut(dev);
    let desc = hbm_description::from(desc);
    let extent = hbm_extent::into(extent);
    let dmabuf = dmabuf_from(dmabuf);
    let layout = hbm_layout::into(layout);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return ptr::null_mut(),
    };

    let bo = match hbm::Bo::with_dma_buf(dev.device.clone(), class, extent, dmabuf, layout) {
        Ok(bo) => bo,
        _ => return ptr::null_mut(),
    };

    hbm_bo::from(bo)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_destroy(bo: *mut hbm_bo) {
    let _ = hbm_bo::into(bo);
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_export_dma_buf(bo: *mut hbm_bo, name: *const ffi::c_char) -> i32 {
    let bo = hbm_bo::as_ref(bo);
    let name = str_as_ref(name);

    let dmabuf = match bo.export_dma_buf(name) {
        Ok(dmabuf) => dmabuf,
        _ => return -1,
    };

    dmabuf_into(dmabuf)
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_layout(bo: *mut hbm_bo, out_layout: *mut hbm_layout) -> bool {
    let bo = hbm_bo::as_ref(bo);
    let out_layout = hbm_layout::as_mut(out_layout);

    let layout = match bo.layout() {
        Ok(layout) => layout,
        _ => return false,
    };

    out_layout.size = layout.size;
    out_layout.modifier = layout.modifier.0;
    out_layout.plane_count = layout.plane_count;
    out_layout.offsets = layout.offsets;
    out_layout.strides = layout.strides;

    true
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_map(bo: *mut hbm_bo) -> *mut ffi::c_void {
    let bo = hbm_bo::as_mut(bo);

    let mapping = match bo.map() {
        Ok(mapping) => mapping,
        _ => return ptr::null_mut(),
    };

    mapping.ptr.as_ptr()
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_unmap(bo: *mut hbm_bo) {
    let bo = hbm_bo::as_mut(bo);

    bo.unmap();
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_flush(bo: *mut hbm_bo) {
    let bo = hbm_bo::as_ref(bo);

    let _ = bo.flush();
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_invalidate(bo: *mut hbm_bo) {
    let bo = hbm_bo::as_ref(bo);

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
    let bo = hbm_bo::as_ref(bo);
    let src = hbm_bo::as_ref(src);

    let copy = hbm::CopyBuffer {
        src_offset,
        dst_offset,
        size,
    };
    // TODO takes an in-fence
    match bo.copy_buffer(src, copy, None) {
        Ok(sync_fd) => {
            if let Some(_sync_fd) = sync_fd {
                // TODO returns the out-fence such that minigbm can DMA_BUF_IOCTL_IMPORT_SYNC_FILE
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
    let bo = hbm_bo::as_ref(bo);
    let src = hbm_bo::as_ref(src);

    let copy = hbm::CopyBufferImage {
        offset,
        stride,
        plane,
        x,
        y,
        width,
        height,
    };

    // TODO takes an in-fence
    match bo.copy_buffer_image(src, copy, None) {
        Ok(sync_fd) => {
            if let Some(_sync_fd) = sync_fd {
                // TODO returns the out-fence such that minigbm can DMA_BUF_IOCTL_IMPORT_SYNC_FILE
            }

            true
        }
        _ => false,
    }
}
