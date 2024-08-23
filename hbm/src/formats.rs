// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Constraint, Layout};
use super::types::{Error, Format, Modifier, Result, Size};
use ash::vk;
use std::{slice, str};

// from drm_fourcc.h
mod consts {
    macro_rules! fourcc_code {
        ($a:literal, $b:literal, $c:literal, $d:literal) => {
            ($a as u32) | (($b as u32) << 8) | (($c as u32) << 16) | (($d as u32) << 24)
        };
    }

    macro_rules! fourcc_mod_code {
        ($vendor:ident, $val:expr) => {
            (($vendor as u64) << 56) | (($val as u64) & ((1 << 56) - 1))
        };
    }

    pub const DRM_FORMAT_INVALID: u32 = 0;
    pub const DRM_FORMAT_R8: u32 = fourcc_code!('R', '8', ' ', ' ');
    pub const DRM_FORMAT_BGR565: u32 = fourcc_code!('B', 'G', '1', '6');
    pub const DRM_FORMAT_RGB565: u32 = fourcc_code!('R', 'G', '1', '6');
    pub const DRM_FORMAT_GR88: u32 = fourcc_code!('G', 'R', '8', '8');
    pub const DRM_FORMAT_R16: u32 = fourcc_code!('R', '1', '6', ' ');
    pub const DRM_FORMAT_BGR888: u32 = fourcc_code!('B', 'G', '2', '4');
    pub const DRM_FORMAT_RGB888: u32 = fourcc_code!('R', 'G', '2', '4');
    pub const DRM_FORMAT_ABGR8888: u32 = fourcc_code!('A', 'B', '2', '4');
    pub const DRM_FORMAT_XBGR8888: u32 = fourcc_code!('X', 'B', '2', '4');
    pub const DRM_FORMAT_ARGB8888: u32 = fourcc_code!('A', 'R', '2', '4');
    pub const DRM_FORMAT_XRGB8888: u32 = fourcc_code!('X', 'R', '2', '4');
    pub const DRM_FORMAT_ABGR2101010: u32 = fourcc_code!('A', 'B', '3', '0');
    pub const DRM_FORMAT_XBGR2101010: u32 = fourcc_code!('X', 'B', '3', '0');
    pub const DRM_FORMAT_ARGB2101010: u32 = fourcc_code!('A', 'R', '3', '0');
    pub const DRM_FORMAT_XRGB2101010: u32 = fourcc_code!('X', 'R', '3', '0');
    pub const DRM_FORMAT_ABGR16161616F: u32 = fourcc_code!('A', 'B', '4', 'H');
    pub const DRM_FORMAT_YUYV: u32 = fourcc_code!('Y', 'U', 'Y', 'V');
    pub const DRM_FORMAT_UYVY: u32 = fourcc_code!('U', 'Y', 'V', 'Y');
    pub const DRM_FORMAT_NV12: u32 = fourcc_code!('N', 'V', '1', '2');
    pub const DRM_FORMAT_NV21: u32 = fourcc_code!('N', 'V', '2', '1');
    pub const DRM_FORMAT_P010: u32 = fourcc_code!('P', '0', '1', '0');
    pub const DRM_FORMAT_P016: u32 = fourcc_code!('P', '0', '1', '6');
    pub const DRM_FORMAT_YUV420: u32 = fourcc_code!('Y', 'U', '1', '2');
    pub const DRM_FORMAT_YVU420: u32 = fourcc_code!('Y', 'V', '1', '2');

    const DRM_FORMAT_MOD_VENDOR_NONE: u64 = 0;
    const DRM_FORMAT_RESERVED: u64 = (1u64 << 56) - 1;

    pub const DRM_FORMAT_MOD_INVALID: u64 =
        fourcc_mod_code!(DRM_FORMAT_MOD_VENDOR_NONE, DRM_FORMAT_RESERVED);
    pub const DRM_FORMAT_MOD_LINEAR: u64 = fourcc_mod_code!(DRM_FORMAT_MOD_VENDOR_NONE, 0);
}

pub const INVALID: Format = Format(consts::DRM_FORMAT_INVALID);
#[cfg(test)]
pub const R8: Format = Format(consts::DRM_FORMAT_R8);

pub const MOD_INVALID: Modifier = Modifier(consts::DRM_FORMAT_MOD_INVALID);
pub const MOD_LINEAR: Modifier = Modifier(consts::DRM_FORMAT_MOD_LINEAR);

const KNOWN_FORMATS: [Format; 24] = [
    Format(consts::DRM_FORMAT_R8),
    Format(consts::DRM_FORMAT_BGR565),
    Format(consts::DRM_FORMAT_RGB565),
    Format(consts::DRM_FORMAT_GR88),
    Format(consts::DRM_FORMAT_R16),
    Format(consts::DRM_FORMAT_BGR888),
    Format(consts::DRM_FORMAT_RGB888),
    Format(consts::DRM_FORMAT_ABGR8888),
    Format(consts::DRM_FORMAT_XBGR8888),
    Format(consts::DRM_FORMAT_ARGB8888),
    Format(consts::DRM_FORMAT_XRGB8888),
    Format(consts::DRM_FORMAT_ABGR2101010),
    Format(consts::DRM_FORMAT_XBGR2101010),
    Format(consts::DRM_FORMAT_ARGB2101010),
    Format(consts::DRM_FORMAT_XRGB2101010),
    Format(consts::DRM_FORMAT_ABGR16161616F),
    Format(consts::DRM_FORMAT_YUYV),
    Format(consts::DRM_FORMAT_UYVY),
    Format(consts::DRM_FORMAT_NV12),
    Format(consts::DRM_FORMAT_NV21),
    Format(consts::DRM_FORMAT_P010),
    Format(consts::DRM_FORMAT_P016),
    Format(consts::DRM_FORMAT_YUV420),
    Format(consts::DRM_FORMAT_YVU420),
];

pub fn fourcc(fmt: Format) -> String {
    let bytes = fmt.0.to_le_bytes();
    if let Ok(s) = str::from_utf8(&bytes) {
        format!("'{s}'")
    } else {
        format!("0x{:x}", fmt.0)
    }
}

pub fn name(fmt: Format) -> Option<&'static str> {
    let name = match fmt.0 {
        consts::DRM_FORMAT_R8 => "R8",
        consts::DRM_FORMAT_BGR565 => "BGR565",
        consts::DRM_FORMAT_RGB565 => "RGB565",
        consts::DRM_FORMAT_GR88 => "GR88",
        consts::DRM_FORMAT_R16 => "R16",
        consts::DRM_FORMAT_BGR888 => "BGR888",
        consts::DRM_FORMAT_RGB888 => "RGB888",
        consts::DRM_FORMAT_ABGR8888 => "ABGR8888",
        consts::DRM_FORMAT_XBGR8888 => "XBGR8888",
        consts::DRM_FORMAT_ARGB8888 => "ARGB8888",
        consts::DRM_FORMAT_XRGB8888 => "XRGB8888",
        consts::DRM_FORMAT_ABGR2101010 => "ABGR2101010",
        consts::DRM_FORMAT_XBGR2101010 => "XBGR2101010",
        consts::DRM_FORMAT_ARGB2101010 => "ARGB2101010",
        consts::DRM_FORMAT_XRGB2101010 => "XRGB2101010",
        consts::DRM_FORMAT_ABGR16161616F => "ABGR16161616F",
        consts::DRM_FORMAT_YUYV => "YUYV",
        consts::DRM_FORMAT_UYVY => "UYVY",
        consts::DRM_FORMAT_NV12 => "NV12",
        consts::DRM_FORMAT_NV21 => "NV21",
        consts::DRM_FORMAT_P010 => "P010",
        consts::DRM_FORMAT_P016 => "P016",
        consts::DRM_FORMAT_YUV420 => "YUV420",
        consts::DRM_FORMAT_YVU420 => "YVU420",
        _ => {
            return None;
        }
    };

    Some(name)
}

struct FormatClass {
    plane_count: u8,
    block_size: [u8; 3],
    block_extent: [(u8, u8); 3],
}

fn format_class(fmt: Format) -> Result<&'static FormatClass> {
    // this follows Vulkan format compatibility classes
    const FORMAT_CLASS_1B: FormatClass = FormatClass {
        plane_count: 1,
        block_size: [1, 0, 0],
        block_extent: [(1, 1), (1, 1), (1, 1)],
    };
    const FORMAT_CLASS_2B: FormatClass = FormatClass {
        block_size: [2, 0, 0],
        ..FORMAT_CLASS_1B
    };
    const FORMAT_CLASS_3B: FormatClass = FormatClass {
        block_size: [3, 0, 0],
        ..FORMAT_CLASS_1B
    };
    const FORMAT_CLASS_4B: FormatClass = FormatClass {
        block_size: [4, 0, 0],
        ..FORMAT_CLASS_1B
    };
    const FORMAT_CLASS_8B: FormatClass = FormatClass {
        block_size: [8, 0, 0],
        ..FORMAT_CLASS_1B
    };
    const FORMAT_CLASS_1PLANE_422_4B: FormatClass = FormatClass {
        block_extent: [(2, 1), (1, 1), (1, 1)],
        ..FORMAT_CLASS_4B
    };
    const FORMAT_CLASS_2PLANE_420_3B: FormatClass = FormatClass {
        plane_count: 2,
        block_size: [1, 2, 0],
        block_extent: [(1, 1), (2, 2), (1, 1)],
    };
    const FORMAT_CLASS_2PLANE_420_6B: FormatClass = FormatClass {
        block_size: [2, 4, 0],
        ..FORMAT_CLASS_2PLANE_420_3B
    };
    const FORMAT_CLASS_3PLANE_420_3B: FormatClass = FormatClass {
        plane_count: 3,
        block_size: [1, 1, 1],
        block_extent: [(1, 1), (2, 2), (2, 2)],
    };

    let fmt_class = match fmt.0 {
        consts::DRM_FORMAT_R8 => &FORMAT_CLASS_1B,
        consts::DRM_FORMAT_BGR565
        | consts::DRM_FORMAT_RGB565
        | consts::DRM_FORMAT_GR88
        | consts::DRM_FORMAT_R16 => &FORMAT_CLASS_2B,
        consts::DRM_FORMAT_BGR888 | consts::DRM_FORMAT_RGB888 => &FORMAT_CLASS_3B,
        consts::DRM_FORMAT_ABGR8888
        | consts::DRM_FORMAT_XBGR8888
        | consts::DRM_FORMAT_ARGB8888
        | consts::DRM_FORMAT_XRGB8888
        | consts::DRM_FORMAT_ABGR2101010
        | consts::DRM_FORMAT_XBGR2101010
        | consts::DRM_FORMAT_ARGB2101010
        | consts::DRM_FORMAT_XRGB2101010 => &FORMAT_CLASS_4B,
        consts::DRM_FORMAT_ABGR16161616F => &FORMAT_CLASS_8B,
        consts::DRM_FORMAT_YUYV | consts::DRM_FORMAT_UYVY => &FORMAT_CLASS_1PLANE_422_4B,
        consts::DRM_FORMAT_NV12 | consts::DRM_FORMAT_NV21 => &FORMAT_CLASS_2PLANE_420_3B,
        consts::DRM_FORMAT_P010 | consts::DRM_FORMAT_P016 => &FORMAT_CLASS_2PLANE_420_6B,
        consts::DRM_FORMAT_YUV420 | consts::DRM_FORMAT_YVU420 => &FORMAT_CLASS_3PLANE_420_3B,
        _ => return Err(Error::InvalidParam),
    };

    Ok(fmt_class)
}

pub fn block_size(fmt: Format, plane: u32) -> Result<u32> {
    let fmt_class = format_class(fmt)?;
    Ok(fmt_class.block_size[plane as usize] as u32)
}

pub fn plane_count(fmt: Format) -> Result<u32> {
    let fmt_class = format_class(fmt)?;
    Ok(fmt_class.plane_count as u32)
}

pub fn packed_layout(
    fmt: Format,
    width: u32,
    height: u32,
    con: Option<Constraint>,
) -> Result<Layout> {
    let fmt_class = format_class(fmt)?;

    let mut layout = Layout::new()
        .modifier(MOD_LINEAR)
        .plane_count(fmt_class.plane_count as u32);

    let (offset_align, stride_align, size_align) = Constraint::unpack(con);
    let mut offset: Size = 0;
    for plane in 0..(fmt_class.plane_count as usize) {
        let (bw, bh) = fmt_class.block_extent[plane];
        let bs = fmt_class.block_size[plane] as Size;

        let width = width.div_ceil(bw as u32) as Size;
        let height = height.div_ceil(bh as u32) as Size;

        offset = offset.next_multiple_of(offset_align);

        let mut stride = width * bs;
        stride = stride.next_multiple_of(stride_align);

        let mut size = stride * height;
        size = size.next_multiple_of(size_align);

        layout.offsets[plane] = offset;
        layout.strides[plane] = stride;
        offset += size;
    }

    layout.size = offset;

    Ok(layout)
}

#[derive(PartialEq)]
pub enum Swizzle {
    None,
    Rgb1,
    Bgra,
}

pub fn to_vk(fmt: Format) -> Result<(vk::Format, Swizzle)> {
    let mapped = match fmt.0 {
        consts::DRM_FORMAT_R8 => (vk::Format::R8_UNORM, Swizzle::None),
        consts::DRM_FORMAT_BGR565 => {
            if cfg!(target_endian = "little") {
                (vk::Format::B5G6R5_UNORM_PACK16, Swizzle::None)
            } else {
                (vk::Format::R5G6B5_UNORM_PACK16, Swizzle::None)
            }
        }
        consts::DRM_FORMAT_RGB565 => {
            if cfg!(target_endian = "little") {
                (vk::Format::R5G6B5_UNORM_PACK16, Swizzle::None)
            } else {
                (vk::Format::B5G6R5_UNORM_PACK16, Swizzle::None)
            }
        }
        consts::DRM_FORMAT_GR88 => (vk::Format::R8G8_UNORM, Swizzle::None),
        consts::DRM_FORMAT_R16 => (vk::Format::R16_UNORM, Swizzle::None),
        consts::DRM_FORMAT_BGR888 => (vk::Format::R8G8B8_UNORM, Swizzle::None),
        consts::DRM_FORMAT_RGB888 => (vk::Format::B8G8R8_UNORM, Swizzle::None),
        consts::DRM_FORMAT_ABGR8888 => (vk::Format::R8G8B8A8_UNORM, Swizzle::None),
        consts::DRM_FORMAT_XBGR8888 => (vk::Format::R8G8B8A8_UNORM, Swizzle::Rgb1),
        consts::DRM_FORMAT_ARGB8888 => (vk::Format::B8G8R8A8_UNORM, Swizzle::None),
        consts::DRM_FORMAT_XRGB8888 => (vk::Format::B8G8R8A8_UNORM, Swizzle::Rgb1),
        consts::DRM_FORMAT_ABGR2101010 => {
            if cfg!(target_endian = "little") {
                (vk::Format::A2B10G10R10_UNORM_PACK32, Swizzle::None)
            } else {
                (vk::Format::UNDEFINED, Swizzle::None)
            }
        }
        consts::DRM_FORMAT_XBGR2101010 => {
            if cfg!(target_endian = "little") {
                (vk::Format::A2B10G10R10_UNORM_PACK32, Swizzle::Rgb1)
            } else {
                (vk::Format::UNDEFINED, Swizzle::None)
            }
        }
        consts::DRM_FORMAT_ARGB2101010 => {
            if cfg!(target_endian = "little") {
                (vk::Format::A2R10G10B10_UNORM_PACK32, Swizzle::None)
            } else {
                (vk::Format::UNDEFINED, Swizzle::None)
            }
        }
        consts::DRM_FORMAT_XRGB2101010 => {
            if cfg!(target_endian = "little") {
                (vk::Format::A2R10G10B10_UNORM_PACK32, Swizzle::Rgb1)
            } else {
                (vk::Format::UNDEFINED, Swizzle::None)
            }
        }
        consts::DRM_FORMAT_ABGR16161616F => (vk::Format::R16G16B16A16_SFLOAT, Swizzle::None),
        consts::DRM_FORMAT_YUYV => (vk::Format::G8B8G8R8_422_UNORM, Swizzle::None),
        consts::DRM_FORMAT_UYVY => (vk::Format::B8G8R8G8_422_UNORM, Swizzle::None),
        consts::DRM_FORMAT_NV12 => (vk::Format::G8_B8R8_2PLANE_420_UNORM, Swizzle::None),
        consts::DRM_FORMAT_NV21 => (vk::Format::G8_B8R8_2PLANE_420_UNORM, Swizzle::Bgra),
        consts::DRM_FORMAT_P010 => (
            vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
            Swizzle::None,
        ),
        consts::DRM_FORMAT_P016 => (vk::Format::G16_B16R16_2PLANE_420_UNORM, Swizzle::None),
        consts::DRM_FORMAT_YUV420 => (vk::Format::G8_B8_R8_3PLANE_420_UNORM, Swizzle::None),
        consts::DRM_FORMAT_YVU420 => (vk::Format::G8_B8_R8_3PLANE_420_UNORM, Swizzle::Bgra),
        _ => (vk::Format::UNDEFINED, Swizzle::None),
    };

    if mapped.0 != vk::Format::UNDEFINED {
        Ok(mapped)
    } else {
        Err(Error::InvalidParam)
    }
}

pub fn from_vk(vk_fmt: vk::Format) -> Format {
    // maybe sash should cache the reverse-mapping
    for fmt in KNOWN_FORMATS {
        if to_vk(fmt).unwrap().0 == vk_fmt {
            return fmt;
        }
    }
    unreachable!()
}

pub struct VkIter(slice::Iter<'static, Format>);

impl Iterator for VkIter {
    type Item = (vk::Format, u8);

    fn next(&mut self) -> Option<Self::Item> {
        for &fmt in self.0.by_ref() {
            if let Ok((vk_fmt, swizzle)) = to_vk(fmt) {
                // We want to return all unique vk formats.  It suffices to return all unswizzled
                // formats given how to_vk works for now.
                if swizzle != Swizzle::None {
                    continue;
                }

                let plane_count = format_class(fmt).unwrap().plane_count;
                return Some((vk_fmt, plane_count));
            }
        }

        None
    }
}

pub fn enumerate_vk() -> VkIter {
    VkIter(KNOWN_FORMATS.iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(test)]
    #[test]
    fn consts() {
        assert_eq!(consts::DRM_FORMAT_INVALID, 0);
        assert_eq!(consts::DRM_FORMAT_R8, 538982482);
        assert_eq!(consts::DRM_FORMAT_MOD_INVALID, 72057594037927935);
        assert_eq!(consts::DRM_FORMAT_MOD_LINEAR, 0);
    }

    #[test]
    fn fourcc() {
        assert_eq!(super::fourcc(R8), String::from("'R8  '"));
        assert_eq!(
            super::fourcc(Format(0xffffffff)),
            String::from("0xffffffff")
        );
    }

    #[test]
    fn name() {
        assert_eq!(super::name(R8), Some("R8"));
        assert_eq!(super::name(INVALID), None);
    }

    #[test]
    fn format_class() {
        for fmt in KNOWN_FORMATS {
            assert!(super::format_class(fmt).is_ok());
        }
    }

    #[test]
    fn block_size() {
        assert_eq!(super::block_size(R8, 0).unwrap(), 1);
    }

    #[test]
    fn packed_layout() {
        let w = 10;
        let h = 10;
        let mut layout = Layout::new()
            .size((w * h) as Size)
            .modifier(MOD_LINEAR)
            .plane_count(1)
            .stride(0, w as Size);
        assert_eq!(super::packed_layout(R8, w, h, None).unwrap(), layout);

        let stride = 16;
        let con = Constraint::new().stride_align(stride);
        layout.size = stride * (h as Size);
        layout.strides[0] = stride;
        assert_eq!(super::packed_layout(R8, w, h, Some(con)).unwrap(), layout);
    }

    #[test]
    fn to_vk() {
        #[cfg(target_endian = "little")]
        for fmt in KNOWN_FORMATS {
            let (vk_fmt, _) = super::to_vk(fmt).unwrap();
            assert_ne!(vk_fmt, vk::Format::UNDEFINED);
        }
    }

    #[test]
    fn enumerate_vk() {
        let mut vk_fmts: Vec<vk::Format> = super::enumerate_vk().map(|(f, _)| f).collect();

        let mut all_vk_fmts = Vec::new();
        for fmt in KNOWN_FORMATS {
            if let Ok((vk_fmt, _)) = super::to_vk(fmt) {
                all_vk_fmts.push(vk_fmt);
            }
        }

        vk_fmts.sort();
        all_vk_fmts.sort();
        all_vk_fmts.dedup();
        assert_eq!(vk_fmts, all_vk_fmts);
    }
}
