// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::{Class, Constraint, Description, Extent, Handle, Layout};
use crate::dma_buf;
use crate::formats;
use crate::types::{Error, Format, Modifier, Result, Size};
use crate::utils;
use drm::buffer::{Buffer as DrmBuffer, DrmFourcc};
use drm::control::{plane, Device as DrmControlDevice};
use drm::Device as DrmDevice;
use std::collections::HashMap;
use std::ops::{Bound, RangeBounds};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub struct Usage: u32 {
        const OVERLAY = 1 << 0;
        const CURSOR = 1 << 1;
    }
}

pub fn open_drm_primary_device(
    node_path: Option<PathBuf>,
    device_id: Option<u64>,
) -> Result<OwnedFd> {
    for path in utils::drm_scan_primary()? {
        if let Some(node_path) = &node_path {
            if *node_path != path {
                continue;
            }
        }
        if let Some(device_id) = device_id {
            if !path.metadata().is_ok_and(|s| device_id == s.rdev()) {
                continue;
            }
        }

        return utils::open(&path);
    }

    Err(Error::NoSupport)
}

pub fn decode_in_formats(table: &mut HashMap<Format, Vec<Modifier>>, data: &[u8]) {
    if let Ok(iter) = utils::drm_parse_in_formats_blob(data) {
        for (modifier, format) in iter {
            let modifiers = table.entry(Format(format)).or_default();

            if !modifiers.iter().any(|m| m.0 == modifier) {
                modifiers.push(Modifier(modifier));
            }
        }
    }
}

fn get_drm_usage(usage: super::Usage) -> Result<Usage> {
    let usage = match usage {
        super::Usage::DrmKms(usage) => usage,
        _ => return Err(Error::InvalidParam),
    };

    if !usage.bits().is_power_of_two() {
        return Err(Error::InvalidParam);
    }

    Ok(usage)
}

fn get_supported_modifiers(
    table: &HashMap<Format, Vec<Modifier>>,
    format: Format,
    modifier: Modifier,
) -> Result<Vec<Modifier>> {
    let mods = table.get(&format).ok_or(Error::NoSupport)?;

    if modifier.is_invalid() {
        Ok(mods.clone())
    } else {
        if !mods.iter().any(|&m| m == modifier) {
            return Err(Error::NoSupport);
        }

        Ok(vec![modifier])
    }
}

struct Device(OwnedFd);

impl AsFd for Device {
    fn as_fd(&self) -> BorrowedFd {
        self.0.as_fd()
    }
}
impl DrmDevice for Device {}
impl DrmControlDevice for Device {}

pub struct Backend {
    device: Device,
    alloc_only: bool,

    max_width: u32,
    max_height: u32,
    overlay_formats: HashMap<Format, Vec<Modifier>>,
    cursor_formats: HashMap<Format, Vec<Modifier>>,
}

impl Backend {
    fn new(fd: OwnedFd, alloc_only: bool) -> Result<Self> {
        let mut backend = Backend {
            device: Device(fd),
            alloc_only,
            max_width: 0,
            max_height: 0,
            overlay_formats: HashMap::new(),
            cursor_formats: HashMap::new(),
        };

        if !backend.alloc_only {
            backend.init()?;
        }

        Ok(backend)
    }

    fn init(&mut self) -> Result<()> {
        self.device
            .set_client_capability(drm::ClientCapability::UniversalPlanes, true)?;

        self.init_max_size()?;

        let res = self.device.plane_handles()?;
        for plane_handle in res {
            self.init_plane(plane_handle)?;
        }

        Ok(())
    }

    fn init_max_size(&mut self) -> Result<()> {
        let get_val = |b: Bound<&u32>| match b {
            Bound::Included(&v) => v,
            Bound::Excluded(&v) => {
                if v > 0 {
                    v - 1
                } else {
                    0
                }
            }
            Bound::Unbounded => 65536,
        };

        let res = self.device.resource_handles()?;
        self.max_width = get_val(res.supported_fb_width().end_bound());
        self.max_height = get_val(res.supported_fb_height().end_bound());

        Ok(())
    }

    fn init_plane(&mut self, plane: plane::Handle) -> Result<()> {
        let info = self.device.get_plane(plane)?;

        let mut plane_type = None;
        let mut in_formats = None;

        let props = self.device.get_properties(info.handle())?;
        for (id, val) in props {
            let prop = match self.device.get_property(id) {
                Ok(prop) => prop,
                _ => continue,
            };

            let name = prop.name().to_str().unwrap();
            match prop.value_type() {
                drm::control::property::ValueType::Enum(_) => {
                    if name == "type" {
                        plane_type = Some(val);
                    }
                }
                drm::control::property::ValueType::Blob => {
                    if name == "IN_FORMATS" {
                        in_formats = Some(self.device.get_property_blob(val)?);
                    }
                }
                _ => {}
            }
            if plane_type.is_some() && in_formats.is_some() {
                break;
            }
        }

        if plane_type.is_none() {
            return Ok(());
        }

        let plane_type = plane_type.unwrap();
        let table = if plane_type == drm::control::PlaneType::Cursor as u64 {
            &mut self.cursor_formats
        } else {
            &mut self.overlay_formats
        };

        if let Some(data) = in_formats {
            decode_in_formats(table, &data);
        } else {
            for fmt in info.formats() {
                table
                    .entry(Format(*fmt))
                    .or_default()
                    .push(formats::MOD_INVALID);
            }
        }

        Ok(())
    }
}

impl super::Backend for Backend {
    fn classify(&self, desc: Description, usage: super::Usage) -> Result<Class> {
        if desc.is_buffer() {
            return Err(Error::NoSupport);
        }

        let drm_usage = get_drm_usage(usage)?;

        let table = if drm_usage.contains(Usage::OVERLAY) {
            &self.overlay_formats
        } else {
            &self.cursor_formats
        };
        let mods = get_supported_modifiers(table, desc.format, desc.modifier)?;

        let class = Class::new(desc)
            .usage(usage)
            .max_extent(Extent::new_2d(self.max_width, self.max_height))
            .modifiers(mods);

        Ok(class)
    }

    fn allocate(&self, class: &Class, extent: Extent, con: Option<Constraint>) -> Result<Handle> {
        let desc = &class.description;

        assert!(!desc.is_buffer());

        let size = (extent.width(), extent.height());
        let fmt = DrmFourcc::try_from(desc.format.0).or(Err(Error::NoSupport))?;
        let bpp = formats::block_size(desc.format, 0)? * 8;

        let buf = self.device.create_dumb_buffer(size, fmt, bpp)?;
        let pitch = buf.pitch();
        let dmabuf = self
            .device
            .buffer_to_prime_fd(buf.handle(), drm::RDWR | drm::CLOEXEC)?;
        // TODO impl Drop, otherwise it leaks if the export above fails
        self.device.destroy_dumb_buffer(buf)?;

        let layout = Layout::new()
            .size((extent.height() * pitch) as Size)
            .modifier(formats::MOD_LINEAR)
            .plane_count(1)
            .stride(0, pitch as Size);
        if !layout.fit(con) {
            return Err(Error::NoSupport);
        }

        let mut res = dma_buf::Resource::new(layout);
        res.bind(dmabuf);

        let handle = Handle::from(res);

        Ok(handle)
    }
}

#[derive(Default)]
pub struct Builder {
    node_path: Option<PathBuf>,
    node_fd: Option<OwnedFd>,
    device_id: Option<u64>,
    alloc_only: bool,
}

impl Builder {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn node_path(mut self, node_path: impl AsRef<Path>) -> Self {
        self.node_path = Some(PathBuf::from(node_path.as_ref()));
        self
    }

    pub fn node_fd(mut self, node_fd: OwnedFd) -> Self {
        self.node_fd = Some(node_fd);
        self
    }

    // st_rdev
    pub fn device_id(mut self, device_id: u64) -> Self {
        self.device_id = Some(device_id);
        self
    }

    pub fn alloc_only(mut self, alloc_only: bool) -> Self {
        self.alloc_only = alloc_only;
        self
    }

    pub fn build(self) -> Result<Backend> {
        if self.node_path.is_some() as i32
            + self.node_fd.is_some() as i32
            + self.device_id.is_some() as i32
            > 1
        {
            return Err(Error::InvalidParam);
        }

        if !utils::drm_exists() {
            return Err(Error::NoSupport);
        }

        let node_fd = if let Some(fd) = self.node_fd {
            fd
        } else {
            open_drm_primary_device(self.node_path, self.device_id)?
        };

        Backend::new(node_fd, self.alloc_only)
    }
}
