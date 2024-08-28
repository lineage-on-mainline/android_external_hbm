// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

//! Simple types.
//!
//! This module defines simple HBM-specific types.

use super::formats;
use std::{ffi, fmt, io, num, ptr, result};

/// The error type for HBM operations.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// A generic error with a descriptive message to provide the context.
    #[error("{0}")]
    Context(&'static str),
    /// A validation error indicating a bad user input.
    #[error("bad user input")]
    User,
    /// Indicates an unsupported operation.
    #[error("unsupported")]
    Unsupported,
    /// A runtime device error that may or may no be persistent.
    #[error("device error")]
    Device,
    #[error("{0}")]
    /// A generic IO error.
    Io(#[from] io::Error),
    #[error("error code {0}")]
    /// A backend-specific opaque error code.
    Code(i32),
    /// A validation error indicating a bad integer.
    #[error("bad integer conversion")]
    IntegerConversion,
    /// A validation error indicating a bad string.
    #[error("bad string conversion")]
    StringConversion,
}

impl Error {
    pub(crate) fn ctx<T>(s: &'static str) -> Result<T> {
        Err(Error::Context(s))
    }

    pub(crate) fn user<T>() -> Result<T> {
        Err(Error::User)
    }

    pub(crate) fn unsupported<T>() -> Result<T> {
        Err(Error::Unsupported)
    }

    pub(crate) fn device<T>() -> Result<T> {
        Err(Error::Device)
    }

    pub(crate) fn errno<T>(err: nix::Error) -> Result<T> {
        Err(Error::Io(io::Error::from(err)))
    }
}

impl From<num::TryFromIntError> for Error {
    fn from(_err: num::TryFromIntError) -> Self {
        Self::IntegerConversion
    }
}

impl From<ffi::NulError> for Error {
    fn from(_err: ffi::NulError) -> Self {
        Self::StringConversion
    }
}

impl From<nix::Error> for Error {
    fn from(err: nix::Error) -> Self {
        Self::from(io::Error::from(err))
    }
}

#[cfg(feature = "ash")]
impl From<ash::vk::Result> for Error {
    fn from(err: ash::vk::Result) -> Self {
        Self::Code(err.as_raw())
    }
}

/// A specialized `Result` type for HBM operations.
pub(crate) type Result<T> = result::Result<T, Error>;

/// The type for the BO size.
pub type Size = u64;

/// A 32-bit DRM format.
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

/// A 64-bit DRM format modifier.
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

/// An access type for memory mapping.
pub(crate) enum Access {
    Read,
    #[allow(dead_code)]
    Write,
    ReadWrite,
}

/// A memory mapping.
#[derive(Clone, Copy)]
pub struct Mapping {
    /// Pointer of a mapping.
    pub ptr: ptr::NonNull<ffi::c_void>,
    /// Size of a mapping.
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
