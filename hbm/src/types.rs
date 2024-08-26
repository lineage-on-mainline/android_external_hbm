// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::formats;
use std::{ffi, fmt, io, num, ptr, result};

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    #[error("no support")]
    NoSupport,
    #[error("invalid parameter")]
    InvalidParam,
    #[error("device io")]
    DeviceIo(#[from] io::Error),
    #[error("loading error")]
    LoadingError,
}

impl From<ffi::NulError> for Error {
    fn from(err: ffi::NulError) -> Self {
        Self::from(io::Error::from(err))
    }
}

impl From<num::TryFromIntError> for Error {
    fn from(_err: num::TryFromIntError) -> Self {
        Self::InvalidParam
    }
}

impl From<nix::Error> for Error {
    fn from(err: nix::Error) -> Self {
        Self::from(io::Error::from(err))
    }
}

#[cfg(feature = "ash")]
impl From<ash::LoadingError> for Error {
    fn from(_err: ash::LoadingError) -> Self {
        Self::LoadingError
    }
}

#[cfg(feature = "ash")]
impl From<ash::vk::Result> for Error {
    fn from(_err: ash::vk::Result) -> Self {
        Self::LoadingError
    }
}

pub(crate) type Result<T> = result::Result<T, Error>;

pub type Size = u64;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Format(pub u32);

impl Format {
    pub(crate) fn is_invalid(&self) -> bool {
        *self == formats::INVALID
    }
}

impl Default for Format {
    fn default() -> Self {
        formats::INVALID
    }
}

impl<T> From<T> for Format
where
    T: Into<u32>,
{
    fn from(val: T) -> Self {
        Self(val.into())
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(name) = formats::name(*self) {
            write!(f, "{}", name)
        } else {
            write!(f, "{}", formats::fourcc(*self))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Modifier(pub u64);

impl Modifier {
    pub(crate) fn is_invalid(&self) -> bool {
        *self == formats::MOD_INVALID
    }

    pub(crate) fn is_linear(&self) -> bool {
        *self == formats::MOD_LINEAR
    }
}

impl Default for Modifier {
    fn default() -> Self {
        formats::MOD_INVALID
    }
}

impl<T> From<T> for Modifier
where
    T: Into<u64>,
{
    fn from(val: T) -> Self {
        Self(val.into())
    }
}

pub(crate) enum Access {
    Read,
    #[allow(dead_code)]
    Write,
    ReadWrite,
}

#[derive(Clone, Copy)]
pub struct Mapping {
    pub ptr: ptr::NonNull<ffi::c_void>,
    pub len: num::NonZeroUsize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format() {
        assert_eq!(Format::default(), formats::INVALID);
    }

    #[test]
    fn test_modifier() {
        assert_eq!(Modifier::default(), formats::MOD_INVALID);
    }
}
