// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use std::collections::{hash_map::Entry, HashMap};
use std::sync::{Arc, Mutex};
use std::{ffi, ptr, slice};

/// Log level of a message or the message filter.
#[repr(C)]
pub enum hbm_log_level {
    /// A pseudo level used to disable all messages.
    Off,
    /// Indicates a failure of a mandatory operation.
    Error,
    /// Indicates a failure of an optional operation.
    Warn,
    /// Indicates an informative message.
    Info,
    /// Indicates a debug message.
    Debug,
}

/// A message log callback.
#[allow(non_camel_case_types)]
pub type hbm_log_callback = Option<
    unsafe extern "C" fn(lv: hbm_log_level, msg: *const ffi::c_char, cb_data: *mut ffi::c_void),
>;

/// The BO can be mapped.
pub const HBM_RESOURCE_FLAG_MAP: u32 = 1 << 0;
/// The BO can be copied to or copied from.
pub const HBM_RESOURCE_FLAG_COPY: u32 = 1 << 1;
/// The BO must be allocated from a protected heap.
pub const HBM_RESOURCE_FLAG_PROTECTED: u32 = 1 << 2;
/// The BO must not be compressed.
pub const HBM_RESOURCE_FLAG_NO_COMPRESSION: u32 = 1 << 3;

/// The BO can be used for GPU copies.
pub const HBM_USAGE_GPU_TRANSFER: u64 = 1u64 << 0;
/// The BO can be used as a GPU uniform buffer.
pub const HBM_USAGE_GPU_UNIFORM: u64 = 1u64 << 1;
/// The BO can be used as a GPU storage buffer or image.
pub const HBM_USAGE_GPU_STORAGE: u64 = 1u64 << 2;
/// The BO can be used as a GPU sampled image.
pub const HBM_USAGE_GPU_SAMPLED: u64 = 1u64 << 3;
/// The BO can be used as a GPU color image.
pub const HBM_USAGE_GPU_COLOR: u64 = 1u64 << 4;
/// The BO can be scanned out.
pub const HBM_USAGE_GPU_SCANOUT_HACK: u64 = 1 << 5;

/// The memory type is local to the backend.
pub const HBM_MEMORY_FLAG_LOCAL: u32 = 1 << 0;
/// The memory type is mappable.
pub const HBM_MEMORY_FLAG_MAPPABLE: u32 = 1 << 1;
/// The memory type is coherent.
pub const HBM_MEMORY_FLAG_COHERENT: u32 = 1 << 2;
/// The memory type is cached.
pub const HBM_MEMORY_FLAG_CACHED: u32 = 1 << 3;

/// A hardware device.
///
/// This opaque struct represents a device.  There are module-level functions to query device info
/// and allocate BOs from the device.
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
    fn classify(&self, desc: &hbm_description) -> Result<hbm::Class, hbm::Error> {
        let usage = hbm::Usage::Vulkan(c::usage(desc.usage));
        let desc = hbm::Description::new()
            .flags(c::res_flags(desc.flags))
            .format(hbm::Format(desc.format))
            .modifier(hbm::Modifier(desc.modifier));

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

/// A hardware buffer object (BO).
///
/// This opaque struct represents a BO.  A BO can be allocated by HBM or imported from a dma-buf.
/// A BO can only be manipulated with module-level functions.
#[repr(C)]
pub struct hbm_bo {
    _data: [u8; 0],
}

/// Describes a BO.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct hbm_description {
    /// A bitmask of `HBM_RESOURCE_FLAG_*`.
    pub flags: u32,

    /// When the format is `DRM_FORMAT_INVALID`, the BO is a buffer.  Otherwise,
    /// the BO is an image.
    pub format: u32,

    /// The modifier can be `DRM_FORMAT_MOD_INVALID` or any valid modifier.  When it is
    /// `DRM_FORMAT_MOD_INVALID`, HBM will pick the optimal modifier for the BO.
    pub modifier: u64,

    /// A bitmask of `HBM_USAGE_*`.
    pub usage: u64,
}

/// Extent of a buffer BO.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct hbm_extent_buffer {
    /// Size of the buffer in bytes.
    pub size: u64,
}

/// Extent of an image BO.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct hbm_extent_image {
    /// Width of the image in texels.
    pub width: u32,
    /// Height of the image in texels.
    pub height: u32,
}

/// Extent of a BO.
#[repr(C)]
pub union hbm_extent {
    /// Used when the BO is a buffer.
    pub buffer: hbm_extent_buffer,
    /// Used when the BO is an image.
    pub image: hbm_extent_image,
}

/// An allocation constraint.
///
/// An allocation constraint describes additional requirements that a BO allocation must obey.
#[repr(C)]
pub struct hbm_constraint {
    /// Alignment for plane offsets in bytes.
    pub offset_align: u64,
    /// Alignment for row strides in bytes.
    pub stride_align: u64,
    /// Alignment for plane sizes in bytes.
    pub size_align: u64,

    /// An optional array of allowed modifiers.
    pub modifiers: *const u64,
    /// The size of the optional allowed modifier array.
    pub modifier_count: u32,
}

/// Describes the physical layout of a BO.
#[repr(C)]
pub struct hbm_layout {
    /// Size of the BO in bytes.
    pub size: u64,
    /// Format modifier.
    pub modifier: u64,
    /// Memory plane count, which can be equal to or greater than the format plane count.
    pub plane_count: u32,
    /// Plane offsets.
    pub offsets: [u64; 4],
    /// Plane row strides.
    pub strides: [u64; 4],
}

/// Describes a buffer-buffer copy.
#[repr(C)]
pub struct hbm_copy_buffer {
    /// Starting offset of the source buffer in bytes.
    pub src_offset: u64,
    /// Starting offset of the destination buffer in bytes.
    pub dst_offset: u64,
    /// Number of bytes to copy.
    pub size: u64,
}

/// Describes a buffer-image copy.
#[repr(C)]
pub struct hbm_copy_buffer_image {
    /// Starting offset of the buffer in bytes.
    pub offset: u64,
    /// Row stride of buffer in bytes.
    pub stride: u64,

    /// Plane of the image to copy.
    pub plane: u32,
    /// Starting X coordinate of the image in texels.
    pub x: u32,
    /// Starting Y coordinate of the image in texels.
    pub y: u32,
    /// Number of texels in X coordinate to copy.
    pub width: u32,
    /// Number of texels in Y coordinate to copy.
    pub height: u32,
}

mod c {
    use super::*;
    use std::os::fd::{BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};

    pub fn log_lv_max(log_lv_max: hbm_log_level) -> log::LevelFilter {
        match log_lv_max {
            hbm_log_level::Off => log::LevelFilter::Off,
            hbm_log_level::Error => log::LevelFilter::Error,
            hbm_log_level::Warn => log::LevelFilter::Warn,
            hbm_log_level::Info => log::LevelFilter::Info,
            hbm_log_level::Debug => log::LevelFilter::Debug,
        }
    }

    pub fn log_lv_ret(log_lv: log::Level) -> hbm_log_level {
        match log_lv {
            log::Level::Error => hbm_log_level::Error,
            log::Level::Warn => hbm_log_level::Warn,
            log::Level::Info => hbm_log_level::Info,
            log::Level::Debug => hbm_log_level::Debug,
            log::Level::Trace => hbm_log_level::Debug,
        }
    }

    pub fn dev_ret(dev: CDevice) -> *mut hbm_device {
        let dev = Box::new(dev);
        Box::into_raw(dev) as *mut hbm_device
    }

    pub fn dev_take(dev: *mut hbm_device) -> Box<CDevice> {
        // SAFETY: dev was created by dev_ret
        unsafe { Box::from_raw(dev as *mut CDevice) }
    }

    pub fn dev<'a>(dev: *mut hbm_device) -> &'a mut CDevice {
        // SAFETY: dev was created by dev_ret
        unsafe { &mut *(dev as *mut CDevice) }
    }

    pub fn desc(desc: *const hbm_description) -> hbm_description {
        // SAFETY: desc is non-NULL
        unsafe { *desc }
    }

    pub fn res_flags(res_flags: u32) -> hbm::ResourceFlags {
        let mut flags = hbm::ResourceFlags::empty();
        if (res_flags & HBM_RESOURCE_FLAG_MAP) > 0 {
            flags |= hbm::ResourceFlags::MAP;
        }
        if (res_flags & HBM_RESOURCE_FLAG_COPY) > 0 {
            flags |= hbm::ResourceFlags::COPY;
        }
        if (res_flags & HBM_RESOURCE_FLAG_PROTECTED) > 0 {
            flags |= hbm::ResourceFlags::PROTECTED;
        }
        if (res_flags & HBM_RESOURCE_FLAG_NO_COMPRESSION) > 0 {
            flags |= hbm::ResourceFlags::NO_COMPRESSION;
        }

        flags
    }

    pub fn usage(usage: u64) -> hbm::vulkan::Usage {
        let mut vk_usage = hbm::vulkan::Usage::empty();
        if (usage & HBM_USAGE_GPU_TRANSFER) > 0 {
            vk_usage |= hbm::vulkan::Usage::TRANSFER;
        }
        if (usage & HBM_USAGE_GPU_UNIFORM) > 0 {
            vk_usage |= hbm::vulkan::Usage::UNIFORM;
        }
        if (usage & HBM_USAGE_GPU_STORAGE) > 0 {
            vk_usage |= hbm::vulkan::Usage::STORAGE;
        }
        if (usage & HBM_USAGE_GPU_SAMPLED) > 0 {
            vk_usage |= hbm::vulkan::Usage::SAMPLED;
        }
        if (usage & HBM_USAGE_GPU_COLOR) > 0 {
            vk_usage |= hbm::vulkan::Usage::COLOR;
        }
        if (usage & HBM_USAGE_GPU_SCANOUT_HACK) > 0 {
            vk_usage |= hbm::vulkan::Usage::SCANOUT_HACK;
        }

        vk_usage
    }

    pub fn mods_out(out_mods: *mut u64, mod_max: u32, mods: &Vec<hbm::Modifier>) -> u32 {
        let mut mod_count = mods.len() as u32;
        if mod_max == 0 {
            return mod_count;
        }

        if mod_count > mod_max {
            mod_count = mod_max;
        }

        // SAFETY: out_mods is large enough for mod_max modifiers
        let out_mods = unsafe { slice::from_raw_parts_mut(out_mods, mod_count as _) };

        for (dst, src) in out_mods.iter_mut().zip(mods.into_iter()) {
            *dst = src.0;
        }

        mod_count
    }

    pub fn extent(extent: *const hbm_extent) -> hbm::Extent {
        // SAFETY: extent is non-NULL
        let extent = unsafe { &*extent };
        // SAFETY: we just need the raw bits
        let size = unsafe { extent.buffer.size };

        hbm::Extent::new_1d(size)
    }

    pub fn con_optional(con: *const hbm_constraint) -> Option<hbm::Constraint> {
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

    pub fn layout(layout: *const hbm_layout) -> hbm::Layout {
        // SAFETY: layout is non-NULL
        let layout = unsafe { &*layout };

        hbm::Layout::new()
            .size(layout.size)
            .modifier(hbm::Modifier(layout.modifier))
            .plane_count(layout.plane_count)
            .offsets(layout.offsets)
            .strides(layout.strides)
    }

    pub fn layout_out(out_layout: *mut hbm_layout, layout: hbm::Layout) {
        // SAFETY: out_layout is non_NULL
        let out_layout = unsafe { &mut *out_layout };

        *out_layout = hbm_layout {
            size: layout.size,
            modifier: layout.modifier.0,
            plane_count: layout.plane_count,
            offsets: layout.offsets,
            strides: layout.strides,
        };
    }

    pub fn bo_ret(bo: hbm::Bo) -> *mut hbm_bo {
        let bo = Box::new(bo);
        Box::into_raw(bo) as *mut hbm_bo
    }

    pub fn bo_take(bo: *mut hbm_bo) -> Box<hbm::Bo> {
        // SAFETY: bo was created by bo_ret
        unsafe { Box::from_raw(bo as *mut hbm::Bo) }
    }

    pub fn bo<'a>(bo: *mut hbm_bo) -> &'a hbm::Bo {
        // SAFETY: bo was created by bo_ret
        unsafe { &*(bo as *const hbm::Bo) }
    }

    pub fn bo_mut<'a>(bo: *mut hbm_bo) -> &'a mut hbm::Bo {
        // SAFETY: bo was created by bo_ret
        unsafe { &mut *(bo as *mut hbm::Bo) }
    }

    pub fn mem_flags(mem_flags: u32) -> hbm::MemoryFlags {
        let mut flags = hbm::MemoryFlags::empty();
        if (mem_flags & HBM_MEMORY_FLAG_LOCAL) > 0 {
            flags |= hbm::MemoryFlags::LOCAL;
        }
        if (mem_flags & HBM_MEMORY_FLAG_MAPPABLE) > 0 {
            flags |= hbm::MemoryFlags::MAPPABLE;
        }
        if (mem_flags & HBM_MEMORY_FLAG_COHERENT) > 0 {
            flags |= hbm::MemoryFlags::COHERENT;
        }
        if (mem_flags & HBM_MEMORY_FLAG_CACHED) > 0 {
            flags |= hbm::MemoryFlags::CACHED;
        }

        flags
    }

    pub fn mem_flags_ret(flags: hbm::MemoryFlags) -> u32 {
        let mut mem_flags = 0;
        if flags.contains(hbm::MemoryFlags::LOCAL) {
            mem_flags |= HBM_MEMORY_FLAG_LOCAL;
        }
        if flags.contains(hbm::MemoryFlags::MAPPABLE) {
            mem_flags |= HBM_MEMORY_FLAG_MAPPABLE;
        }
        if flags.contains(hbm::MemoryFlags::COHERENT) {
            mem_flags |= HBM_MEMORY_FLAG_COHERENT;
        }
        if flags.contains(hbm::MemoryFlags::CACHED) {
            mem_flags |= HBM_MEMORY_FLAG_CACHED;
        }

        mem_flags
    }

    pub fn mem_flags_out(out_mts: *mut u32, mt_max: u32, mts: Vec<hbm::MemoryFlags>) -> u32 {
        let mut mt_count = mts.len() as u32;
        if mt_max == 0 {
            return mt_count;
        }

        if mt_count > mt_max {
            mt_count = mt_max;
        }

        // SAFETY: out_mts is large enough for mt_max memory types
        let out_mts = unsafe { slice::from_raw_parts_mut(out_mts, mt_count as _) };

        for (dst, src) in out_mts.iter_mut().zip(mts.into_iter()) {
            *dst = mem_flags_ret(src);
        }

        mt_count
    }

    pub fn fd_borrow<'a>(fd: RawFd) -> Option<BorrowedFd<'a>> {
        if fd < 0 {
            return None;
        }

        // SAFETY: fd is valid
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        Some(fd)
    }

    pub fn fd_optional(fd: RawFd) -> Option<OwnedFd> {
        if fd < 0 {
            return None;
        }

        // SAFETY: fd is valid
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Some(fd)
    }

    pub fn fd_ret(fd: OwnedFd) -> RawFd {
        fd.into_raw_fd()
    }

    pub fn fd_out(out_fd: *mut RawFd, fd: Option<OwnedFd>) {
        // SAFETY: out_fd is non-NULL
        let out_fd = unsafe { &mut *out_fd };
        *out_fd = fd.map_or(-1, |fd| fd.into_raw_fd());
    }

    pub fn str_optional<'a>(s: *const ffi::c_char) -> Option<&'a str> {
        if s.is_null() {
            return None;
        }

        // SAFETY: s is a non-NULL and nul-terminated
        let s = unsafe { ffi::CStr::from_ptr(s) };

        s.to_str().ok()
    }

    pub fn buf_copy(copy: *const hbm_copy_buffer) -> hbm::CopyBuffer {
        // SAFETY: copy is non-NULL
        let copy = unsafe { &*copy };

        hbm::CopyBuffer {
            src_offset: copy.src_offset,
            dst_offset: copy.dst_offset,
            size: copy.size,
        }
    }

    pub fn img_copy(copy: *const hbm_copy_buffer_image) -> hbm::CopyBufferImage {
        // SAFETY: copy is non-NULL
        let copy = unsafe { &*copy };

        hbm::CopyBufferImage {
            offset: copy.offset,
            stride: copy.stride,
            plane: copy.plane,
            x: copy.x,
            y: copy.y,
            width: copy.width,
            height: copy.height,
        }
    }
}

/// Initializes logging.
///
/// # Safety
///
/// If `log_cb` is non-NULL, it must be a valid callback.
#[no_mangle]
pub unsafe extern "C" fn hbm_log_init(
    log_lv_max: hbm_log_level,
    log_cb: hbm_log_callback,
    cb_data: *mut ffi::c_void,
) {
    let log_lv_max = c::log_lv_max(log_lv_max);
    if log_lv_max == log::LevelFilter::Off || log_cb.is_none() {
        super::log::init(log::LevelFilter::Off, Box::new(|_| {}));
        return;
    }

    let log_cb = log_cb.unwrap();
    let cb_data = cb_data as usize;
    let cb = move |rec: &log::Record| {
        let log_lv = c::log_lv_ret(rec.level());
        let msg = format!("{}", rec.args());

        let _ = ffi::CString::new(msg).inspect(|cstr|
            // SAFETY: we trust the client
            unsafe {
                log_cb(log_lv, cstr.as_ptr(), cb_data as *mut ffi::c_void);
            });
    };

    super::log::init(log_lv_max, Box::new(cb));
}

/// Creates a device.
///
/// # Safety
///
/// This function is always safe.
#[no_mangle]
pub unsafe extern "C" fn hbm_device_create(dev: libc::dev_t, debug: bool) -> *mut hbm_device {
    let backend = match hbm::vulkan::Builder::new()
        .device_id(dev)
        .debug(debug)
        .build()
    {
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

    c::dev_ret(dev)
}

/// Destroys a device.
///
/// # Safety
///
/// `dev` must be a valid device.
#[no_mangle]
pub unsafe extern "C" fn hbm_device_destroy(dev: *mut hbm_device) {
    let _ = c::dev_take(dev);
}

/// Queries the memory plane count for the speicifed format modifier.
///
/// # Safety
///
/// `dev` must be a valid device.
#[no_mangle]
pub unsafe extern "C" fn hbm_device_get_plane_count(
    dev: *mut hbm_device,
    fmt: u32,
    modifier: u64,
) -> u32 {
    let dev = c::dev(dev);

    dev.device
        .plane_count(hbm::Format(fmt), hbm::Modifier(modifier))
        .unwrap_or(0)
}

/// Queries supported modifiers for a BO description.
///
/// # Safety
///
/// `dev` must be a valid device.
///
/// `desc` must be non-NULL.
///
/// If `out_mods` is non-NULL, it must point to a large enough array of at least `mod_max` elements.
#[no_mangle]
pub unsafe extern "C" fn hbm_device_get_modifiers(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    mod_max: u32,
    out_mods: *mut u64,
) -> i32 {
    let dev = c::dev(dev);
    let desc = c::desc(desc);

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

    c::mods_out(out_mods, mod_max, mods) as i32
}

/// Queries modifier support for a BO description.
///
/// # Safety
///
/// `dev` must be a valid device.
///
/// `desc` must be non-NULL.
#[no_mangle]
pub unsafe extern "C" fn hbm_device_supports_modifier(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    modifier: u64,
) -> bool {
    let dev = c::dev(dev);
    let desc = c::desc(desc);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return false,
    };

    dev.device
        .modifiers(class)
        .map(|mods| mods.iter().any(|m| m.0 == modifier))
        .unwrap_or(false)
}

/// Create a BO with a constraint.
///
/// `con` is optional.
///
/// # Safety
///
/// `dev` must be a valid device.
///
/// `desc` and `extent` must be non-NULL.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_create_with_constraint(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    con: *const hbm_constraint,
) -> *mut hbm_bo {
    let dev = c::dev(dev);
    let desc = c::desc(desc);
    let extent = c::extent(extent);
    let con = c::con_optional(con);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return ptr::null_mut(),
    };

    let bo = match hbm::Bo::with_constraint(dev.device.clone(), class, extent, con) {
        Ok(bo) => bo,
        _ => return ptr::null_mut(),
    };

    c::bo_ret(bo)
}

/// Create a BO with an explicit layout.
///
/// # Safety
///
/// `dev` must be a valid device.
///
/// `desc`, `extent`, and `layout` must be non-NULL.
///
/// `dmabuf` must be a valid dma-buf.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_create_with_layout(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    layout: *const hbm_layout,
    dmabuf: i32,
) -> *mut hbm_bo {
    let dev = c::dev(dev);
    let desc = c::desc(desc);
    let extent = c::extent(extent);
    let layout = c::layout(layout);
    let dmabuf = c::fd_borrow(dmabuf);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return ptr::null_mut(),
    };

    let bo = match hbm::Bo::with_layout(dev.device.clone(), class, extent, layout, dmabuf) {
        Ok(bo) => bo,
        _ => return ptr::null_mut(),
    };

    c::bo_ret(bo)
}

/// Destroys a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_destroy(bo: *mut hbm_bo) {
    let _ = c::bo_take(bo);
}

/// Queries the physical layout of a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_layout(bo: *mut hbm_bo, out_layout: *mut hbm_layout) -> bool {
    let bo = c::bo(bo);

    let layout = match bo.layout() {
        Ok(layout) => layout,
        _ => return false,
    };

    c::layout_out(out_layout, layout);

    true
}

/// Queries supported memory types of a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_memory_types(
    bo: *mut hbm_bo,
    mt_max: u32,
    out_mts: *mut u32,
) -> u32 {
    let bo = c::bo(bo);

    let mts = bo.memory_types();

    c::mem_flags_out(out_mts, mt_max, mts)
}

/// Bind memory to a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_bind_memory(bo: *mut hbm_bo, mem_flags: u32, dmabuf: i32) -> bool {
    let bo = c::bo_mut(bo);
    let mem_flags = c::mem_flags(mem_flags);
    let dmabuf = c::fd_optional(dmabuf);

    bo.bind_memory(mem_flags, dmabuf).is_ok()
}

/// Exports a dma-buf from a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_export_dma_buf(bo: *mut hbm_bo, name: *const ffi::c_char) -> i32 {
    let bo = c::bo(bo);
    let name = c::str_optional(name);

    let dmabuf = match bo.export_dma_buf(name) {
        Ok(dmabuf) => dmabuf,
        _ => return -1,
    };

    c::fd_ret(dmabuf)
}

/// Map a BO for direct CPU access.
///
/// The BO must have `HBM_FLAG_MAP`.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_map(bo: *mut hbm_bo) -> *mut ffi::c_void {
    let bo = c::bo_mut(bo);

    let mapping = match bo.map() {
        Ok(mapping) => mapping,
        _ => return ptr::null_mut(),
    };

    mapping.ptr.as_ptr()
}

/// Unmap a mapped BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_unmap(bo: *mut hbm_bo) {
    let bo = c::bo_mut(bo);

    bo.unmap();
}

/// Flush the mapping of a mapped BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_flush(bo: *mut hbm_bo) {
    let bo = c::bo(bo);

    let _ = bo.flush();
}

/// Invalidate the mapping of a mapped BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_invalidate(bo: *mut hbm_bo) {
    let bo = c::bo(bo);

    let _ = bo.invalidate();
}

/// Performs a buffer-buffer copy between two BOs.
///
/// This function copies the contents from `src` to `bo`.  Both BOs must be buffers and must have
/// `HBM_FLAG_COPY`.
///
/// If `in_sync_fd` is non-negative, it must be a valid sync fd and its ownership is transferred to
/// this function.  The copy starts after the sync fd signals.
///
/// If `out_sync_fd` is non-NULL, it is set to a valid sync fd or -1.  If it is set to a valid sync
/// fd, the copy ends after the sync fd signals.  If `out_sync_fd` is NULL or if it is set to -1,
/// the copy ends before this function returns.
///
/// # Safety
///
/// `bo` and `src` must be valid BOs belonging to the same device.
///
/// `copy` must be non-NULL.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_copy_buffer(
    bo: *mut hbm_bo,
    src: *mut hbm_bo,
    copy: *const hbm_copy_buffer,
    in_sync_fd: i32,
    out_sync_fd: *mut i32,
) -> bool {
    let bo = c::bo(bo);
    let src = c::bo(src);
    let copy = c::buf_copy(copy);
    let in_sync_fd = c::fd_optional(in_sync_fd);
    let wait = out_sync_fd.is_null();

    let sync_fd = bo.copy_buffer(src, copy, in_sync_fd, wait);
    if sync_fd.is_err() {
        return false;
    }

    if !wait {
        c::fd_out(out_sync_fd, sync_fd.unwrap());
    }

    true
}

/// Performs a buffer-image copy between two BOs.
///
/// This function copies the contents from `src` to `bo`.  One of them must be a buffer and the
/// other must be an image.  Both must have `HBM_FLAG_COPY`.
///
/// If `in_sync_fd` is non-negative, it must be a valid sync fd and its ownership is transferred to
/// this function.  The copy starts after the sync fd signals.
///
/// If `out_sync_fd` is non-NULL, it is set to a valid sync fd or -1.  If it is set to a valid sync
/// fd, the copy ends after the sync fd signals.  If `out_sync_fd` is NULL or if it is set to -1,
/// the copy ends before this function returns.
///
/// # Safety
///
/// `bo` and `src` must be valid BOs belonging to the same device.
///
/// `copy` must be non-NULL.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_copy_buffer_image(
    bo: *mut hbm_bo,
    src: *mut hbm_bo,
    copy: *const hbm_copy_buffer_image,
    in_sync_fd: i32,
    out_sync_fd: *mut i32,
) -> bool {
    let bo = c::bo(bo);
    let src = c::bo(src);
    let copy = c::img_copy(copy);
    let in_sync_fd = c::fd_optional(in_sync_fd);
    let wait = out_sync_fd.is_null();

    let sync_fd = bo.copy_buffer_image(src, copy, in_sync_fd, wait);
    if sync_fd.is_err() {
        return false;
    }

    if !wait {
        c::fd_out(out_sync_fd, sync_fd.unwrap());
    }

    true
}
