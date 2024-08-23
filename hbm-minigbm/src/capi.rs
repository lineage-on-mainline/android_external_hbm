// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use std::collections::{hash_map::Entry, HashMap};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::{ffi, ptr, slice};

/// The BO can be mapped.
pub const HBM_FLAG_MAP: u32 = 1 << 0;
/// The BO can be copied to or copied from.
pub const HBM_FLAG_COPY: u32 = 1 << 1;
/// The BO must be allocated from a protected heap.
pub const HBM_FLAG_PROTECTED: u32 = 1 << 2;
/// The BO must not be compressed.
pub const HBM_FLAG_NO_COMPRESSION: u32 = 1 << 3;
/// The BO can be scanned out.
pub const HBM_FLAG_SCANOUT: u32 = 1 << 4;

/// Prefers a coherent mapping.
pub const HBM_FLAG_COHERENT: u32 = 1 << 5;
/// Prefers a non-cached mapping.
pub const HBM_FLAG_NO_CACHE: u32 = 1 << 6;

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
        unsafe { &mut *(dev as *mut Self) }
    }

    fn classify(&self, desc: &hbm_description) -> Result<hbm::Class, hbm::Error> {
        let mut flags = hbm::ResourceFlags::empty();
        if (desc.flags & HBM_FLAG_MAP) > 0 {
            flags |= hbm::ResourceFlags::MAP;
        }
        if (desc.flags & HBM_FLAG_COPY) > 0 {
            flags |= hbm::ResourceFlags::COPY;
        }
        if (desc.flags & HBM_FLAG_PROTECTED) > 0 {
            flags |= hbm::ResourceFlags::PROTECTED;
        }
        if (desc.flags & HBM_FLAG_NO_COMPRESSION) > 0 {
            flags |= hbm::ResourceFlags::NO_COMPRESSION;
        }
        if (desc.flags & HBM_FLAG_SCANOUT) > 0 {
            flags |= hbm::ResourceFlags::SCANOUT;
        }

        let mut vk_usage = hbm::vulkan::Usage::empty();
        if (desc.usage & HBM_USAGE_GPU_TRANSFER) > 0 {
            vk_usage |= hbm::vulkan::Usage::TRANSFER;
        }
        if (desc.usage & HBM_USAGE_GPU_UNIFORM) > 0 {
            vk_usage |= hbm::vulkan::Usage::UNIFORM;
        }
        if (desc.usage & HBM_USAGE_GPU_STORAGE) > 0 {
            vk_usage |= hbm::vulkan::Usage::STORAGE;
        }
        if (desc.usage & HBM_USAGE_GPU_SAMPLED) > 0 {
            vk_usage |= hbm::vulkan::Usage::SAMPLED;
        }
        if (desc.usage & HBM_USAGE_GPU_COLOR) > 0 {
            vk_usage |= hbm::vulkan::Usage::COLOR;
        }

        let desc = hbm::Description::new()
            .flags(flags)
            .format(hbm::Format(desc.format))
            .modifier(hbm::Modifier(desc.modifier));
        let usage = hbm::Usage::Vulkan(vk_usage);

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

/// Describes a BO.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct hbm_description {
    /// A bitmask of `HBM_FLAG_*`.
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

impl hbm_description {
    fn into(desc: *const Self) -> Self {
        // SAFETY: desc is non-NULL
        unsafe { *desc }
    }
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

impl hbm_extent {
    fn into(extent: *const Self) -> hbm::Extent {
        // SAFETY: extent is non-NULL
        let extent = unsafe { &*extent };
        // SAFETY: we just need the raw bits
        let size = unsafe { extent.buffer.size };

        hbm::Extent::new_1d(size)
    }
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

impl hbm_constraint {
    fn try_into(con: *const Self) -> Option<hbm::Constraint> {
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

/// A hardware buffer object (BO).
///
/// This opaque struct represents a BO.  A BO can be allocated by HBM or imported from a dma-buf.
/// A BO can only be manipulated with module-level functions.
#[repr(C)]
pub struct hbm_bo {
    _data: [u8; 0],
}

impl hbm_bo {
    fn from(bo: hbm::Bo) -> *mut Self {
        let bo = Box::new(bo);
        Box::into_raw(bo) as *mut Self
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

fn rawfd_try_into(fd: RawFd) -> Option<OwnedFd> {
    if fd < 0 {
        return None;
    }

    // SAFETY: fd is valid
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    Some(fd)
}

fn rawfd_from(fd: OwnedFd) -> RawFd {
    fd.into_raw_fd()
}

fn rawfd_as_mut<'a>(fd: *mut RawFd) -> Option<&'a mut RawFd> {
    if fd.is_null() {
        return None;
    }

    // SAFETY: fd is non-NULL
    let fd = unsafe { &mut *fd };
    Some(fd)
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

    fn as_mut<'a>(layout: *mut Self) -> &'a mut Self {
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

impl hbm_copy_buffer {
    fn into(copy: *const Self) -> hbm::CopyBuffer {
        // SAFETY: copy is non-NULL
        let copy = unsafe { &*copy };

        hbm::CopyBuffer {
            src_offset: copy.src_offset,
            dst_offset: copy.dst_offset,
            size: copy.size,
        }
    }
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

impl hbm_copy_buffer_image {
    fn into(copy: *const Self) -> hbm::CopyBufferImage {
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

    CDevice::into(dev)
}

/// Destroys a device.
///
/// # Safety
///
/// `dev` must be a valid device.
#[no_mangle]
pub unsafe extern "C" fn hbm_device_destroy(dev: *mut hbm_device) {
    let _ = CDevice::from(dev);
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
    let dev = CDevice::as_mut(dev);

    match dev
        .device
        .plane_count(hbm::Format(fmt), hbm::Modifier(modifier))
    {
        Ok(count) => count,
        _ => 0,
    }
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
    let dev = CDevice::as_mut(dev);
    let desc = hbm_description::into(desc);

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

    let mut mod_len = mods.len();
    if mod_max > 0 {
        if mod_len > mod_max as _ {
            mod_len = mod_max as _;
        }

        // SAFETY: out_mods is large enough for mod_max modifiers
        let out_mods = unsafe { slice::from_raw_parts_mut(out_mods, mod_len) };

        for (dst, src) in out_mods.iter_mut().zip(mods.iter()) {
            *dst = src.0;
        }
    }

    mod_len as i32
}

/// Allocates a BO.
///
/// `con` is optional.
///
/// # Safety
///
/// `dev` must be a valid device.
///
/// `desc` and `extent` must be non-NULL.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_create(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    con: *const hbm_constraint,
) -> *mut hbm_bo {
    let dev = CDevice::as_mut(dev);
    let desc = hbm_description::into(desc);
    let extent = hbm_extent::into(extent);
    let con = hbm_constraint::try_into(con);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return ptr::null_mut(),
    };

    let mut bo = match hbm::Bo::with_constraint(dev.device.clone(), class, extent, con) {
        Ok(bo) => bo,
        _ => return ptr::null_mut(),
    };

    let mut flags = hbm::MemoryFlags::empty();
    if (desc.flags & HBM_FLAG_MAP) > 0 {
        flags |= hbm::MemoryFlags::MAPPABLE;
        if (desc.flags & HBM_FLAG_COHERENT) > 0 {
            flags |= hbm::MemoryFlags::COHERENT;
        }
        if (desc.flags & HBM_FLAG_NO_CACHE) == 0 {
            flags |= hbm::MemoryFlags::CACHED;
        }
    }

    if bo.bind_memory(flags, None).is_err() {
        return ptr::null_mut();
    }

    hbm_bo::from(bo)
}

/// Imports a dma-buf as a BO.
///
/// # Safety
///
/// `dev` must be a valid device.
///
/// `desc`, `extent`, and `layout` must be non-NULL.
///
/// `dmabuf` must be a valid dma-buf.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_import_dma_buf(
    dev: *mut hbm_device,
    desc: *const hbm_description,
    extent: *const hbm_extent,
    dmabuf: i32,
    layout: *const hbm_layout,
) -> *mut hbm_bo {
    let dev = CDevice::as_mut(dev);
    let desc = hbm_description::into(desc);
    let extent = hbm_extent::into(extent);
    let dmabuf = rawfd_try_into(dmabuf).unwrap();
    let layout = hbm_layout::into(layout);

    let mut class_cache = dev.class_cache.lock().unwrap();
    let class = match dev.get_class(&mut class_cache, desc) {
        Ok(class) => class,
        _ => return ptr::null_mut(),
    };

    let mut bo = match hbm::Bo::with_layout(dev.device.clone(), class, extent, layout) {
        Ok(bo) => bo,
        _ => return ptr::null_mut(),
    };

    let mut flags = hbm::MemoryFlags::empty();
    if (desc.flags & HBM_FLAG_MAP) > 0 {
        flags |= hbm::MemoryFlags::MAPPABLE;
        if (desc.flags & HBM_FLAG_COHERENT) > 0 {
            flags |= hbm::MemoryFlags::COHERENT;
        }
        if (desc.flags & HBM_FLAG_NO_CACHE) == 0 {
            flags |= hbm::MemoryFlags::CACHED;
        }
    }

    if bo.bind_memory(flags, Some(dmabuf)).is_err() {
        return ptr::null_mut();
    }

    hbm_bo::from(bo)
}

/// Destroys a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_destroy(bo: *mut hbm_bo) {
    let _ = hbm_bo::into(bo);
}

/// Exports a dma-buf from a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_export_dma_buf(bo: *mut hbm_bo, name: *const ffi::c_char) -> i32 {
    let bo = hbm_bo::as_ref(bo);
    let name = str_as_ref(name);

    let dmabuf = match bo.export_dma_buf(name) {
        Ok(dmabuf) => dmabuf,
        _ => return -1,
    };

    rawfd_from(dmabuf)
}

/// Queries the physical layout of a BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
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

/// Map a BO for direct CPU access.
///
/// The BO must have `HBM_FLAG_MAP`.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_map(bo: *mut hbm_bo) -> *mut ffi::c_void {
    let bo = hbm_bo::as_mut(bo);

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
    let bo = hbm_bo::as_mut(bo);

    bo.unmap();
}

/// Flush the mapping of a mapped BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_flush(bo: *mut hbm_bo) {
    let bo = hbm_bo::as_ref(bo);

    let _ = bo.flush();
}

/// Invalidate the mapping of a mapped BO.
///
/// # Safety
///
/// `bo` must be a valid BO.
#[no_mangle]
pub unsafe extern "C" fn hbm_bo_invalidate(bo: *mut hbm_bo) {
    let bo = hbm_bo::as_ref(bo);

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
    let bo = hbm_bo::as_ref(bo);
    let src = hbm_bo::as_ref(src);
    let copy = hbm_copy_buffer::into(copy);
    let in_sync_fd = rawfd_try_into(in_sync_fd);
    let out_sync_fd = rawfd_as_mut(out_sync_fd);

    let sync_fd = bo.copy_buffer(src, copy, in_sync_fd, out_sync_fd.is_none());
    if sync_fd.is_err() {
        return false;
    }

    if let Some(out_sync_fd) = out_sync_fd {
        *out_sync_fd = if let Some(sync_fd) = sync_fd.unwrap() {
            rawfd_from(sync_fd)
        } else {
            -1
        }
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
    let bo = hbm_bo::as_ref(bo);
    let src = hbm_bo::as_ref(src);
    let copy = hbm_copy_buffer_image::into(copy);
    let in_sync_fd = rawfd_try_into(in_sync_fd);
    let out_sync_fd = rawfd_as_mut(out_sync_fd);

    let sync_fd = bo.copy_buffer_image(src, copy, in_sync_fd, out_sync_fd.is_none());
    if sync_fd.is_err() {
        return false;
    }

    if let Some(out_sync_fd) = out_sync_fd {
        *out_sync_fd = if let Some(sync_fd) = sync_fd.unwrap() {
            rawfd_from(sync_fd)
        } else {
            -1
        }
    }

    true
}
