// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

pub mod dma_heap;
#[cfg(feature = "drm")]
pub mod drm_kms;
pub mod udmabuf;
pub mod vulkan;

use super::dma_buf;
use super::formats;
use super::sash;
use super::types::{Error, Format, Mapping, Modifier, Result, Size};
use std::os::fd::{BorrowedFd, OwnedFd};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    pub struct Flags: u32 {
        const EXTERNAL = 1 << 0;
        const MAP = 1 << 1;
        const COPY = 1 << 2;
        const PROTECTED = 1 << 3;
        const NO_COMPRESSION = 1 << 4;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct Description {
    pub flags: Flags,
    pub format: Format,
    pub modifier: Modifier,
}

impl Description {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn flags(mut self, flags: Flags) -> Self {
        self.flags = flags;
        self
    }

    pub fn format(mut self, fmt: Format) -> Self {
        self.format = fmt;
        self
    }

    pub fn modifier(mut self, modifier: Modifier) -> Self {
        self.modifier = modifier;
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        if self.is_buffer() {
            self.modifier.is_invalid()
        } else {
            true
        }
    }

    pub(crate) fn is_buffer(&self) -> bool {
        self.format.is_invalid()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Usage {
    Unused,
    #[cfg(feature = "drm")]
    DrmKms(drm_kms::Usage),
    Vulkan(vulkan::Usage),
}

// this is validated and must be opaque to users
#[derive(Clone, Debug)]
pub struct Class {
    // these are copied from user inputs
    pub(crate) flags: Flags,
    pub(crate) format: Format,
    pub(crate) usage: Usage,

    // These express backend limits.  When there are multiple backends, limits from all backends
    // are merged.
    pub(crate) max_extent: Extent,
    pub(crate) modifiers: Vec<Modifier>,
    pub(crate) constraint: Option<Constraint>,
    pub(crate) unknown_constraint: bool,

    // this is set by Device
    pub(crate) backend_index: usize,
}

impl Class {
    pub(crate) fn new(desc: &Description) -> Self {
        Self {
            flags: desc.flags,
            format: desc.format,
            usage: Usage::Unused,
            max_extent: Extent::max(desc.is_buffer()),
            modifiers: Vec::new(),
            constraint: None,
            unknown_constraint: false,
            backend_index: 0,
        }
    }

    pub(crate) fn usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }

    pub(crate) fn max_extent(mut self, max_extent: Extent) -> Self {
        self.max_extent = max_extent;
        self
    }

    pub(crate) fn modifiers(mut self, modifiers: Vec<Modifier>) -> Self {
        self.modifiers = modifiers;
        self
    }

    pub(crate) fn constraint(mut self, con: Constraint) -> Self {
        self.constraint = Some(con);
        self
    }

    pub(crate) fn unknown_constraint(mut self) -> Self {
        self.unknown_constraint = true;
        self
    }

    pub(crate) fn backend_index(mut self, idx: usize) -> Self {
        self.backend_index = idx;
        self
    }

    pub(crate) fn is_buffer(&self) -> bool {
        self.format.is_invalid()
    }

    pub(crate) fn validate(&self, extent: Extent) -> bool {
        if self.is_buffer() {
            let max_size = self.max_extent.size();
            let size = extent.size();

            (1..=max_size).contains(&size)
        } else {
            let max_width = self.max_extent.width();
            let max_height = self.max_extent.height();
            let width = extent.width();
            let height = extent.height();

            (1..=max_width).contains(&width) && (1..=max_height).contains(&height)
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Extent {
    Buffer(Size),
    Image(u32, u32),
}

impl Extent {
    pub(crate) fn max(is_buf: bool) -> Self {
        if is_buf {
            Self::Buffer(u64::MAX)
        } else {
            Self::Image(u32::MAX, u32::MAX)
        }
    }

    pub(crate) fn size(&self) -> Size {
        if let Extent::Buffer(size) = self {
            *size
        } else {
            unreachable!();
        }
    }

    pub(crate) fn width(&self) -> u32 {
        if let Extent::Image(width, _) = self {
            *width
        } else {
            unreachable!();
        }
    }

    pub(crate) fn height(&self) -> u32 {
        if let Extent::Image(_, height) = self {
            *height
        } else {
            unreachable!();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Extent::Buffer(size) => *size == 0,
            Extent::Image(width, height) => *width == 0 || *height == 0,
        }
    }

    pub(crate) fn intersect(&mut self, other: Extent) {
        match self {
            Extent::Buffer(size) => {
                if *size > other.size() {
                    *size = other.size();
                }
            }
            Extent::Image(width, height) => {
                if *width > other.width() {
                    *width = other.width();
                }
                if *height > other.height() {
                    *height = other.height();
                }
            }
        };
    }
}

#[derive(Clone, Debug)]
pub struct Constraint {
    pub(crate) offset_align: Size,
    pub(crate) stride_align: Size,
    pub(crate) size_align: Size,

    pub(crate) modifiers: Vec<Modifier>,
}

impl Default for Constraint {
    fn default() -> Self {
        Self {
            offset_align: 1,
            stride_align: 1,
            size_align: 1,
            modifiers: Default::default(),
        }
    }
}

impl Constraint {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn offset_align(mut self, align: Size) -> Self {
        if align > 1 {
            self.offset_align = align;
        }
        self
    }

    pub fn stride_align(mut self, align: Size) -> Self {
        if align > 1 {
            self.stride_align = align;
        }
        self
    }

    pub fn size_align(mut self, align: Size) -> Self {
        if align > 1 {
            self.size_align = align;
        }
        self
    }

    pub fn modifiers(mut self, modifiers: Vec<Modifier>) -> Self {
        self.modifiers = modifiers;
        self
    }

    fn to_tuple(&self) -> (Size, Size, Size) {
        (self.offset_align, self.stride_align, self.size_align)
    }

    pub(crate) fn merge(&mut self, other: Self) {
        if self.offset_align < other.offset_align {
            assert_eq!(other.offset_align % self.offset_align, 0);
            self.offset_align = other.offset_align;
        }

        if self.stride_align < other.stride_align {
            assert_eq!(other.stride_align % self.stride_align, 0);
            self.stride_align = other.stride_align;
        }

        if self.size_align < other.size_align {
            assert_eq!(other.size_align % self.size_align, 0);
            self.size_align = other.size_align;
        }

        if !other.modifiers.is_empty() {
            assert!(self.modifiers.is_empty());
            self.modifiers = other.modifiers;
        }
    }

    pub(crate) fn unpack(con: Option<Constraint>) -> (Size, Size, Size) {
        con.unwrap_or_default().to_tuple()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Layout {
    pub size: Size,
    pub modifier: Modifier,
    pub plane_count: u32,
    pub offsets: [Size; 4],
    pub strides: [Size; 4],
}

impl Layout {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn modifier(mut self, modifier: Modifier) -> Self {
        self.modifier = modifier;
        self
    }

    pub fn plane_count(mut self, plane_count: u32) -> Self {
        self.plane_count = plane_count;
        self
    }

    pub fn offsets(mut self, offsets: [Size; 4]) -> Self {
        self.offsets = offsets;
        self
    }

    pub fn strides(mut self, strides: [Size; 4]) -> Self {
        self.strides = strides;
        self
    }

    pub fn offset(mut self, plane: usize, offset: Size) -> Self {
        self.offsets[plane] = offset;
        self
    }

    pub fn stride(mut self, plane: usize, stride: Size) -> Self {
        self.strides[plane] = stride;
        self
    }

    pub(crate) fn packed(class: &Class, extent: Extent, con: Option<Constraint>) -> Result<Self> {
        let layout = if class.is_buffer() {
            let (_, _, size_align) = Constraint::unpack(con);
            let size = extent.size().next_multiple_of(size_align);

            Self::new().size(size)
        } else {
            if !class.modifiers.iter().any(|m| m.is_linear()) {
                return Err(Error::InvalidParam);
            }

            formats::packed_layout(class.format, extent.width(), extent.height(), con)?
        };

        Ok(layout)
    }

    pub(crate) fn fit(&self, con: Option<Constraint>) -> bool {
        if con.is_none() {
            return true;
        }
        let con = con.unwrap();

        if con.offset_align > 1 {
            for plane in 0..self.plane_count {
                if self.offsets[plane as usize] % con.offset_align != 0 {
                    return false;
                }
            }
        }

        if con.stride_align > 1 {
            for plane in 0..self.plane_count {
                if self.strides[plane as usize] % con.stride_align != 0 {
                    return false;
                }
            }
        }

        if con.size_align > 1 {
            let count = self.plane_count as usize;

            let mut sorted = self.offsets;
            sorted[..count].sort();
            for plane in 0..count {
                let next_offset = if plane < count - 1 {
                    sorted[plane + 1]
                } else {
                    self.size
                };

                let size = next_offset - self.offsets[plane];
                // it suffices if the plane is large enough
                if size < con.size_align {
                    return false;
                }
            }
        }

        true
    }
}

pub(crate) enum HandlePayload {
    DmaBuf(dma_buf::Resource),
    Buffer(sash::Buffer),
    Image(sash::Image),
}

pub struct Handle {
    pub(crate) payload: HandlePayload,
}

impl Handle {
    pub(crate) fn new(payload: HandlePayload) -> Self {
        Self { payload }
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct MemoryType: u32 {
        const LOCAL = 1 << 0;
        const MAPPABLE = 1 << 1;
        const COHERENT = 1 << 2;
        const CACHED = 1 << 3;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CopyBuffer {
    pub src_offset: Size,
    pub dst_offset: Size,
    pub size: Size,
}

#[derive(Clone, Copy, Debug)]
pub struct CopyBufferImage {
    pub offset: Size,
    pub stride: Size,

    pub plane: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub trait Backend: Send + Sync {
    fn memory_plane_count(&self, _fmt: Format, _modifier: Modifier) -> Result<u32> {
        Err(Error::NoSupport)
    }

    fn classify(&self, desc: Description, usage: Usage) -> Result<Class> {
        dma_buf::classify(desc, usage)
    }

    fn with_constraint(
        &self,
        class: &Class,
        extent: Extent,
        con: Option<Constraint>,
    ) -> Result<Handle> {
        dma_buf::with_constraint(class, extent, con)
    }

    fn with_layout(
        &self,
        class: &Class,
        extent: Extent,
        layout: Layout,
        dmabuf: Option<BorrowedFd>,
    ) -> Result<Handle> {
        dma_buf::with_layout(class, extent, layout, dmabuf)
    }

    fn free(&self, _handle: &Handle) {}

    fn layout(&self, handle: &Handle) -> Layout {
        dma_buf::layout(handle)
    }

    fn memory_types(&self, handle: &Handle) -> Vec<MemoryType> {
        dma_buf::memory_types(handle)
    }

    fn bind_memory(
        &self,
        _handle: &mut Handle,
        _mt: MemoryType,
        _dmabuf: Option<OwnedFd>,
    ) -> Result<()> {
        Err(Error::NoSupport)
    }

    fn export_dma_buf(&self, handle: &Handle, name: Option<&str>) -> Result<OwnedFd> {
        dma_buf::export_dma_buf(handle, name)
    }

    fn map(&self, handle: &Handle) -> Result<Mapping> {
        dma_buf::map(handle)
    }

    fn unmap(&self, handle: &Handle, mapping: Mapping) {
        dma_buf::unmap(handle, mapping)
    }

    fn flush(&self, handle: &Handle) {
        dma_buf::flush(handle);
    }

    fn invalidate(&self, handle: &Handle) {
        dma_buf::invalidate(handle);
    }

    fn copy_buffer(
        &self,
        _dst: &Handle,
        _src: &Handle,
        _copy: CopyBuffer,
        _sync_fd: Option<OwnedFd>,
    ) -> Result<Option<OwnedFd>> {
        Err(Error::NoSupport)
    }

    fn copy_buffer_image(
        &self,
        _dst: &Handle,
        _src: &Handle,
        _copy: CopyBufferImage,
        _sync_fd: Option<OwnedFd>,
    ) -> Result<Option<OwnedFd>> {
        Err(Error::NoSupport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp;

    #[test]
    fn test_description() {
        let mut desc = Description::new();
        assert!(desc.is_valid());
        assert!(desc.is_buffer());

        desc = desc.modifier(formats::MOD_LINEAR);
        assert!(!desc.is_valid());

        desc = desc.format(formats::R8);
        assert!(desc.is_valid());
        assert!(!desc.is_buffer());
    }

    #[test]
    fn test_class() {
        let buf_desc = Description::new();
        let buf_class = Class::new(&buf_desc).max_extent(Extent::Buffer(10));

        assert!(!buf_class.validate(Extent::Buffer(0)));
        assert!(buf_class.validate(Extent::Buffer(1)));

        assert!(buf_class.validate(Extent::Buffer(9)));
        assert!(buf_class.validate(Extent::Buffer(10)));
        assert!(!buf_class.validate(Extent::Buffer(11)));

        let img_desc = Description::new().format(formats::R8);
        let img_class = Class::new(&img_desc).max_extent(Extent::Image(5, 10));

        assert!(!img_class.validate(Extent::Image(0, 0)));
        assert!(!img_class.validate(Extent::Image(5, 0)));
        assert!(!img_class.validate(Extent::Image(0, 10)));
        assert!(img_class.validate(Extent::Image(5, 1)));
        assert!(img_class.validate(Extent::Image(1, 10)));

        assert!(img_class.validate(Extent::Image(5, 10)));
        assert!(!img_class.validate(Extent::Image(6, 10)));
        assert!(!img_class.validate(Extent::Image(5, 11)));
        assert!(!img_class.validate(Extent::Image(6, 11)));
    }

    #[test]
    fn test_extent() {
        for val in [42 as Size, (0x1234 as Size) << 30] {
            assert_eq!(Extent::Buffer(val).size(), val);
        }

        for (w, h) in [(5, 10), (10, 5)] {
            let extent = Extent::Image(w, h);
            assert_eq!(extent.width(), w);
            assert_eq!(extent.height(), h);
        }

        let buf_max = Extent::max(true);
        assert_eq!(buf_max.size(), u64::MAX);

        let img_max = Extent::max(false);
        assert_eq!(img_max.width(), u32::MAX);
        assert_eq!(img_max.height(), u32::MAX);

        assert!(Extent::Buffer(0).is_empty());
        assert!(!Extent::Buffer(1).is_empty());

        assert!(Extent::Image(0, 1).is_empty());
        assert!(Extent::Image(1, 0).is_empty());
        assert!(!Extent::Image(1, 1).is_empty());

        for (v1, v2) in [(0, 10), (10, 0), (5, 10), (10, 5)] {
            let mut extent = Extent::Buffer(v1);
            extent.intersect(Extent::Buffer(v2));
            assert_eq!(extent.size(), cmp::min(v1, v2));
        }

        for ((w1, h1), (w2, h2)) in [((5, 20), (15, 10)), ((0, 20), (15, 0))] {
            let mut extent = Extent::Image(w1, h1);
            extent.intersect(Extent::Image(w2, h2));
            assert_eq!(extent.width(), cmp::min(w1, w2));
            assert_eq!(extent.height(), cmp::min(h1, h2));
        }
    }

    #[test]
    fn test_constraint() {
        let con = Constraint::new();
        assert_eq!(con.to_tuple(), (1, 1, 1));

        let con = Constraint::new()
            .offset_align(0)
            .stride_align(0)
            .size_align(0);
        assert_eq!(con.to_tuple(), (1, 1, 1));

        let con = Constraint::new()
            .offset_align(8)
            .stride_align(16)
            .size_align(32);
        assert_eq!(con.to_tuple(), (8, 16, 32));

        // we don't require power-of-two at the moment
        let con = Constraint::new()
            .offset_align(10)
            .stride_align(11)
            .size_align(12);
        assert_eq!(con.to_tuple(), (10, 11, 12));

        let con = Constraint::new()
            .offset_align(8)
            .stride_align(16)
            .size_align(32);
        assert_eq!(Constraint::unpack(Some(con)), (8, 16, 32));
        assert_eq!(Constraint::unpack(None), (1, 1, 1));
    }

    #[test]
    fn test_layout() {
        let size = 10;
        let buf_desc = Description::new();
        let buf_class = Class::new(&buf_desc).max_extent(Extent::Buffer(size));
        let mut buf_layout = Layout::new().size(size);
        assert_eq!(
            Layout::packed(&buf_class, Extent::Buffer(size), None).unwrap(),
            buf_layout
        );

        let size_align = 16;
        let con = Constraint::new().size_align(size_align);
        buf_layout = buf_layout.size(size.next_multiple_of(size_align));
        assert_eq!(
            Layout::packed(&buf_class, Extent::Buffer(size), Some(con)).unwrap(),
            buf_layout
        );

        let width = 5;
        let height = 10;
        let img_desc = Description::new()
            .format(formats::R8)
            .modifier(formats::MOD_LINEAR);
        let img_class = Class::new(&img_desc)
            .max_extent(Extent::Image(width, height))
            .modifiers(vec![formats::MOD_LINEAR]);
        let mut img_layout = Layout::new()
            .size((width * height) as Size)
            .modifier(formats::MOD_LINEAR)
            .plane_count(1)
            .stride(0, width as Size);
        assert_eq!(
            Layout::packed(&img_class, Extent::Image(width, height), None).unwrap(),
            img_layout
        );

        let stride_align = 8;
        let size_align = 32;
        let con = Constraint::new()
            .stride_align(stride_align)
            .size_align(size_align);

        let aligned_width = (width as Size).next_multiple_of(stride_align);
        let aligned_size = (aligned_width * height as Size).next_multiple_of(size_align);
        img_layout = img_layout.size(aligned_size).stride(0, aligned_width);
        assert_eq!(
            Layout::packed(&img_class, Extent::Image(width, height), Some(con)).unwrap(),
            img_layout
        );

        assert!(img_layout.fit(None));

        // we know img_layout has stride 8 and size 96
        let con = Constraint::new().stride_align(8).size_align(96);
        assert!(img_layout.fit(Some(con)));

        let con = Constraint::new().stride_align(16);
        assert!(!img_layout.fit(Some(con)));

        let con = Constraint::new().size_align(192);
        assert!(!img_layout.fit(Some(con)));

        // for size align, we care about the size itself rather than its real alignment
        let con = Constraint::new().size_align(64);
        assert!(img_layout.fit(Some(con)));
    }
}
