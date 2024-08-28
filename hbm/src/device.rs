// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

//! Device-related types.
//!
//! This module defines `Device` and `Builder`

use super::backends::{Backend, Class, Constraint, Description, Extent, Usage};
use super::types::{Error, Format, Modifier, Result};
use std::collections::HashSet;
use std::sync::Arc;

/// A device.
///
/// A device consists of one or more backends to interact with the underlying subsystems and hardware.
pub struct Device {
    backends: Vec<Box<dyn Backend>>,
}

impl Device {
    /// Returns the memory plane count of a format and a modifier.
    ///
    /// The format plane count is a property of a format.  The memory plane count is a property of
    /// both a format and a modifier.
    ///
    /// When the modifier is `DRM_FORMAT_MOD_LINEAR`, the memory plane count is equal to the format
    /// plane count.  Otherwise, the memory plane count is equal to or greater than the format
    /// plane count.
    pub fn memory_plane_count(&self, fmt: Format, modifier: Modifier) -> Result<u32> {
        if fmt.is_invalid() || modifier.is_invalid() {
            return Error::user();
        }

        for backend in &self.backends {
            match backend.memory_plane_count(fmt, modifier) {
                Err(Error::Unsupported) => (),
                res => return res,
            }
        }

        Error::unsupported()
    }

    /// Creates the opaque BO class for a BO description and a BO usage.
    ///
    /// This validates the BO description and usage and returns the opaque BO class.  If the
    /// possible combinations of BO description/usage are limited, it is suggested to cache the BO
    /// classes to avoid repeated validations.
    pub fn classify(&self, desc: Description, usage: &[Usage]) -> Result<Class> {
        if !desc.is_valid() {
            return Error::user();
        }

        if self.backends.len() != usage.len() {
            return Error::user();
        }

        if self.backends.len() == 1 {
            self.backends[0].classify(desc, usage[0])
        } else {
            // this is unused and needs more work
            self.multi_classify(desc, usage)
        }
    }

    fn multi_classify(&self, desc: Description, usage: &[Usage]) -> Result<Class> {
        // call classify from all backends and merge the results
        let mut max_extent = Extent::max(desc.is_buffer());
        let mut mods: Option<HashSet<Modifier>> = None;
        let mut con = Constraint::new();
        let mut required_idx = None;
        for (idx, (backend, &usage)) in self.backends.iter().zip(usage.iter()).enumerate() {
            if usage == Usage::Unused {
                continue;
            }

            let class = backend.classify(desc, usage)?;

            max_extent.intersect(class.max_extent);

            if !desc.is_buffer() {
                let backend_mods: HashSet<Modifier> = class.modifiers.into_iter().collect();
                mods = Some(match mods {
                    Some(mods) => mods.intersection(&backend_mods).copied().collect(),
                    None => backend_mods,
                });
            }

            if let Some(backend_con) = class.constraint {
                con.merge(backend_con);
            }

            if class.unknown_constraint {
                if required_idx.is_none() {
                    required_idx = Some(idx);
                } else {
                    return Error::unsupported();
                }
            }
        }

        if max_extent.is_empty() {
            return Error::unsupported();
        }

        let mods: Vec<Modifier> = if desc.is_buffer() {
            Vec::new()
        } else {
            let mods = mods.unwrap_or_default();
            if mods.is_empty() {
                return Error::unsupported();
            }

            mods.into_iter().collect()
        };

        let idx = required_idx.unwrap_or(0);
        let class = Class::new(desc)
            .usage(usage[idx])
            .max_extent(max_extent)
            .modifiers(mods)
            .constraint(con)
            .backend_index(idx);

        Ok(class)
    }

    /// Returns the supported modifiers of a BO class.
    pub fn modifiers<'a>(&self, class: &'a Class) -> &'a Vec<Modifier> {
        static EMPTY: Vec<Modifier> = Vec::new();

        // MOD_INVALID indicates an implicit modifier internally, but it means there is no modifier
        // support to users
        //
        // TODO move this to hbm-minigbm?
        if class.modifiers.iter().any(|m| m.is_invalid()) {
            &EMPTY
        } else {
            &class.modifiers
        }
    }

    pub(crate) fn backend(&self, idx: usize) -> &dyn Backend {
        self.backends[idx].as_ref()
    }
}

/// A device builder.
///
/// The sole purpose of a builder is to build a `Device`.
#[derive(Default)]
pub struct Builder {
    backends: Vec<Box<dyn super::Backend>>,
}

impl Builder {
    /// Creates a device builder.
    pub fn new() -> Self {
        Default::default()
    }

    /// Adds a backend to the device builder.
    pub fn add_backend<T>(mut self, backend: T) -> Self
    where
        T: Backend + 'static,
    {
        self.backends.push(Box::new(backend));
        self
    }

    /// Builds a `Device`.
    pub fn build(self) -> Result<Arc<Device>> {
        if self.backends.is_empty() {
            return Error::user();
        }

        let dev = Device {
            backends: self.backends,
        };

        Ok(Arc::new(dev))
    }
}
