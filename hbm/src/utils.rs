// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::types::{Access, Error, Mapping, Result, Size};
use nix::{fcntl, poll, sys, unistd};
use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::{io, num, slice};

pub fn makedev(major: u64, minor: u64) -> u64 {
    sys::stat::makedev(major, minor) as u64
}

pub fn open(path: impl AsRef<Path>) -> Result<OwnedFd> {
    let oflag = fcntl::OFlag::O_RDWR | fcntl::OFlag::O_CLOEXEC;
    let mode = sys::stat::Mode::empty();

    let raw_fd = fcntl::open(path.as_ref(), oflag, mode)?;

    // SAFETY: raw_fd is valid
    let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

    Ok(owned_fd)
}

pub fn seek_end(fd: impl AsFd) -> Result<Size> {
    let offset = unistd::lseek(fd.as_fd().as_raw_fd(), 0, unistd::Whence::SeekEnd)?;
    Ok(offset.try_into()?)
}

pub fn mmap(fd: impl AsFd, size: Size, access: Access) -> Result<Mapping> {
    let prot = match access {
        Access::Read => sys::mman::ProtFlags::PROT_READ,
        Access::Write => sys::mman::ProtFlags::PROT_WRITE,
        Access::ReadWrite => sys::mman::ProtFlags::PROT_READ | sys::mman::ProtFlags::PROT_WRITE,
    };
    let flags = sys::mman::MapFlags::MAP_SHARED;

    let len = num::NonZeroUsize::try_from(usize::try_from(size)?)?;
    let ptr =
        // SAFETY: clients assume the responsibility
        unsafe { sys::mman::mmap(None, len, prot, flags, fd, 0) }?;
    let mapping = Mapping { ptr, len };

    Ok(mapping)
}

pub fn munmap(mapping: Mapping) -> Result<()> {
    // SAFETY: ptr and len are from sys::mman::mmap
    unsafe { sys::mman::munmap(mapping.ptr, mapping.len.into()) }?;

    Ok(())
}

pub fn poll(fd: impl AsFd, access: Access) -> Result<bool> {
    let timeout = poll::PollTimeout::NONE;

    let events = match access {
        Access::Read => poll::PollFlags::POLLIN,
        Access::Write => poll::PollFlags::POLLOUT,
        Access::ReadWrite => poll::PollFlags::POLLIN | poll::PollFlags::POLLOUT,
    };

    loop {
        let mut poll_fd = poll::PollFd::new(fd.as_fd(), events);

        match poll::poll(slice::from_mut(&mut poll_fd), timeout) {
            Ok(ret) => {
                // ret should always be positive because we don't have a timeout
                if ret > 0 {
                    let revents = poll_fd.revents().unwrap_or(poll::PollFlags::POLLNVAL);
                    if !(revents & !events).is_empty() {
                        return Err(Error::from(io::Error::from(nix::Error::EINVAL)));
                    }
                }

                return Ok(ret > 0);
            }
            Err(err) => {
                if err == nix::Error::EINTR || err == nix::Error::EAGAIN {
                    continue;
                }
                return Err(Error::from(io::Error::from(err)));
            }
        }
    }
}

pub fn memfd_create(name: &str, size: Size) -> Result<OwnedFd> {
    let create_flags =
        sys::memfd::MemFdCreateFlag::MFD_CLOEXEC | sys::memfd::MemFdCreateFlag::MFD_ALLOW_SEALING;
    let seal_flags = fcntl::SealFlag::F_SEAL_SHRINK
        | fcntl::SealFlag::F_SEAL_GROW
        | fcntl::SealFlag::F_SEAL_SEAL;
    let fcntl_arg = fcntl::FcntlArg::F_ADD_SEALS(seal_flags);

    let c_name = CString::new(name)?;
    let memfd = sys::memfd::memfd_create(&c_name, create_flags)?;

    unistd::ftruncate(&memfd, size.try_into()?)?;
    fcntl::fcntl(memfd.as_raw_fd(), fcntl_arg)?;

    Ok(memfd)
}

// Based on
//
//   $ bindgen --no-doc-comments --no-layout-tests \
//       --allowlist-item '(dma_buf|DMA_BUF)_.*' \
//       /usr/include/linux/dma-buf.h
mod dma_buf {
    use super::*;

    const DMA_BUF_SYNC_READ: u64 = 1;
    const DMA_BUF_SYNC_WRITE: u64 = 2;
    const DMA_BUF_SYNC_START: u64 = 0;
    const DMA_BUF_SYNC_END: u64 = 4;

    #[repr(C)]
    struct dma_buf_sync {
        pub flags: u64,
    }

    const DMA_BUF_BASE: u8 = b'b';

    nix::ioctl_write_ptr!(dma_buf_ioctl_sync, DMA_BUF_BASE, 0, dma_buf_sync);
    nix::ioctl_write_ptr!(dma_buf_ioctl_set_name, DMA_BUF_BASE, 1, u64);

    pub fn dma_buf_sync(dmabuf: impl AsFd, access: Access, start: bool) -> Result<()> {
        let flags = match access {
            Access::Read => DMA_BUF_SYNC_READ,
            Access::Write => DMA_BUF_SYNC_WRITE,
            Access::ReadWrite => DMA_BUF_SYNC_READ | DMA_BUF_SYNC_WRITE,
        } | match start {
            true => DMA_BUF_SYNC_START,
            false => DMA_BUF_SYNC_END,
        };

        let dmabuf = dmabuf.as_fd().as_raw_fd();
        let arg = dma_buf_sync { flags };
        loop {
            // SAFETY: dmabuf and arg are valid
            let res = unsafe { dma_buf_ioctl_sync(dmabuf, &arg) };
            match res {
                Ok(_) => {
                    return Ok(());
                }
                Err(err) => {
                    if err == nix::Error::EINTR || err == nix::Error::EAGAIN {
                        continue;
                    }
                    return Err(Error::from(io::Error::from(err)));
                }
            }
        }
    }

    pub fn dma_buf_set_name(dmabuf: impl AsFd, name: &str) -> Result<()> {
        let dmabuf = dmabuf.as_fd().as_raw_fd();
        let c_name = CString::new(name)?;

        // SAFETY: dmabuf and c_name are valid
        unsafe { dma_buf_ioctl_set_name(dmabuf, c_name.as_ptr() as *const u64) }?;

        Ok(())
    }
}

pub use dma_buf::{dma_buf_set_name, dma_buf_sync};

// Based on
//
//   $ bindgen --no-doc-comments --no-layout-tests \
//       --allowlist-item '(dma_heap|DMA_HEAP)_.*' \
//       /usr/include/linux/dma-heap.h
mod dma_heap {
    use super::*;
    use std::path::PathBuf;

    #[repr(C)]
    struct dma_heap_allocation_data {
        len: u64,
        fd: u32,
        fd_flags: u32,
        heap_flags: u64,
    }

    const DMA_HEAP_IOC_MAGIC: u8 = b'H';

    nix::ioctl_readwrite!(
        dma_heap_ioctl_alloc,
        DMA_HEAP_IOC_MAGIC,
        0x0,
        dma_heap_allocation_data
    );

    const DMA_HEAP_PATH: &str = "/dev/dma_heap";

    pub fn dma_heap_exists() -> bool {
        Path::new(DMA_HEAP_PATH).try_exists().unwrap_or(true)
    }

    pub fn dma_heap_open(heap_name: &str) -> Result<OwnedFd> {
        let mut path = PathBuf::from(DMA_HEAP_PATH);
        path.push(heap_name);
        open(path)
    }

    pub fn dma_heap_alloc(heap_fd: impl AsFd, size: Size) -> Result<OwnedFd> {
        let fd_flags = (fcntl::OFlag::O_RDWR | fcntl::OFlag::O_CLOEXEC).bits() as u32;
        let mut arg = dma_heap_allocation_data {
            len: size,
            fd: 0,
            fd_flags,
            heap_flags: 0,
        };

        let heap_fd = heap_fd.as_fd().as_raw_fd();
        // SAFETY: heap_fd and arg are valid
        unsafe { dma_heap_ioctl_alloc(heap_fd, &mut arg) }?;

        // SAFETY: arg.fd is valid
        let dmabuf = unsafe { OwnedFd::from_raw_fd(arg.fd as i32) };
        Ok(dmabuf)
    }
}

pub use dma_heap::{dma_heap_alloc, dma_heap_exists, dma_heap_open};

// Based on
//
//   $ bindgen --no-doc-comments --no-layout-tests \
//       --allowlist-item '(udmabuf|UDMABUF)_.*' \
//       /usr/include/linux/udmabuf.h
mod udmabuf {
    use super::*;

    const UDMABUF_FLAGS_CLOEXEC: u32 = 1;

    #[repr(C)]
    struct udmabuf_create {
        memfd: u32,
        flags: u32,
        offset: u64,
        size: u64,
    }

    const UDMABUF_IOC_MAGIC: u8 = b'u';

    nix::ioctl_write_ptr!(
        udmabuf_ioctl_create,
        UDMABUF_IOC_MAGIC,
        0x42,
        udmabuf_create
    );

    const UDMABUF_PATH: &str = "/dev/udmabuf";

    pub fn udmabuf_exists() -> bool {
        Path::new(UDMABUF_PATH).try_exists().unwrap_or(true)
    }

    pub fn udmabuf_open() -> Result<OwnedFd> {
        open(UDMABUF_PATH)
    }

    pub fn udmabuf_alloc(udmabuf_fd: impl AsFd, memfd: OwnedFd, size: Size) -> Result<OwnedFd> {
        let arg = udmabuf_create {
            memfd: memfd.as_raw_fd() as u32,
            flags: UDMABUF_FLAGS_CLOEXEC,
            offset: 0,
            size,
        };

        let udmabuf_fd = udmabuf_fd.as_fd().as_raw_fd();
        // SAFETY: udmabuf_fd and arg are valid
        let raw_fd = unsafe { udmabuf_ioctl_create(udmabuf_fd, &arg) }?;

        // SAFETY: raw_fd is valid
        let dmabuf = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        Ok(dmabuf)
    }
}

pub use udmabuf::{udmabuf_alloc, udmabuf_exists, udmabuf_open};

// Based on
//
//   $ bindgen --no-doc-comments --no-layout-tests \
//       --allowlist-item '(drm|DRM)_.*' \
//       /usr/include/drm/drm_mode.h
#[cfg(feature = "drm")]
mod drm {
    use super::*;
    use std::path::PathBuf;
    use std::{fs, mem};

    #[repr(C)]
    struct drm_format_modifier_blob {
        version: u32,
        flags: u32,
        count_formats: u32,
        formats_offset: u32,
        count_modifiers: u32,
        modifiers_offset: u32,
    }

    #[repr(C)]
    struct drm_format_modifier {
        formats: u64,
        offset: u32,
        pad: u32,
        modifier: u64,
    }

    pub const DRM_DIR_NAME: &str = "/dev/dri";
    pub const DRM_PRIMARY_MINOR_NAME: &str = "card";

    pub fn drm_exists() -> bool {
        Path::new(DRM_DIR_NAME).try_exists().unwrap_or(true)
    }

    pub struct InFormatsIter<'a> {
        formats: &'a [u32],
        modifier_iter: slice::Iter<'a, drm_format_modifier>,

        modifier: u64,
        offset: u32,
        mask: u64,
    }

    impl Iterator for InFormatsIter<'_> {
        type Item = (u64, u32);

        fn next(&mut self) -> Option<Self::Item> {
            while self.mask == 0 {
                // move to the next drm_format_modifier
                if let Some(m) = self.modifier_iter.next() {
                    self.modifier = m.modifier;
                    self.offset = m.offset;
                    self.mask = m.formats;
                } else {
                    return None;
                }
            }

            let bit = self.mask.trailing_zeros();
            let idx = (self.offset + bit) as usize;
            self.mask &= !(1 << bit);

            Some((self.modifier, self.formats[idx]))
        }
    }

    pub fn drm_parse_in_formats_blob(blob: &[u8]) -> Result<InFormatsIter> {
        let hdr_size = mem::size_of::<drm_format_modifier_blob>();
        if hdr_size > blob.len() {
            return Err(Error::InvalidParam);
        }

        let hdr_ptr = blob.as_ptr() as *const drm_format_modifier_blob;
        // SAFETY: hdr_ptr points to a valid header
        let hdr = unsafe { &*hdr_ptr };
        if hdr.version != 1 {
            return Err(Error::InvalidParam);
        }

        let fmt_offset = hdr.formats_offset as usize;
        let fmt_count = hdr.count_formats as usize;
        let fmt_size = mem::size_of::<u32>() * fmt_count;
        if fmt_offset < hdr_size || fmt_offset + fmt_size > blob.len() {
            return Err(Error::InvalidParam);
        }

        // SAFETY: blob is large enough to hold the formats
        let fmt_ptr = unsafe { blob.as_ptr().add(fmt_offset) } as *const u32;
        // SAFETY: blob is large enough to hold the formats
        let formats = unsafe { slice::from_raw_parts(fmt_ptr, fmt_count) };

        let mod_offset = hdr.modifiers_offset as usize;
        let mod_count = hdr.count_modifiers as usize;
        let mod_size = mem::size_of::<u32>() * mod_count;
        if mod_offset < fmt_offset + fmt_size || mod_offset + mod_size > blob.len() {
            return Err(Error::InvalidParam);
        }

        // SAFETY: blob is large enough to hold the modifiers
        let mod_ptr = unsafe { blob.as_ptr().add(mod_offset) } as *const drm_format_modifier;
        // SAFETY: blob is large enough to hold the modifiers
        let modifiers = unsafe { slice::from_raw_parts(mod_ptr, mod_count) };

        let iter = InFormatsIter {
            formats,
            modifier_iter: modifiers.iter(),
            modifier: Default::default(),
            offset: 0,
            mask: 0,
        };

        Ok(iter)
    }

    pub fn drm_scan_primary() -> Result<impl Iterator<Item = PathBuf>> {
        let dir_iter = fs::read_dir(DRM_DIR_NAME)?;
        let primary_iter = dir_iter.filter_map(|entry| {
            if let Ok(entry) = entry {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|s| s.starts_with(DRM_PRIMARY_MINOR_NAME))
                {
                    Some(entry.path())
                } else {
                    None
                }
            } else {
                None
            }
        });

        Ok(primary_iter)
    }
}

#[cfg(feature = "drm")]
pub use drm::{drm_exists, drm_parse_in_formats_blob, drm_scan_primary};
