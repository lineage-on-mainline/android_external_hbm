// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Backend, Class, Constraint, Description, Extent, Usage};
use super::types::{Error, Format, Modifier, Result};
use std::collections::HashSet;
use std::sync::Arc;

pub struct Device {
    pub(crate) backends: Vec<Box<dyn Backend>>,
}

impl Device {
    pub fn memory_plane_count(&self, fmt: Format, modifier: Modifier) -> Result<u32> {
        if fmt.is_invalid() || modifier.is_invalid() {
            return Err(Error::InvalidParam);
        }

        for backend in &self.backends {
            match backend.memory_plane_count(fmt, modifier) {
                Err(Error::NoSupport) => (),
                res => return res,
            }
        }

        Err(Error::NoSupport)
    }

    pub fn classify(&self, desc: Description, usage: &[Usage]) -> Result<Class> {
        if !desc.is_valid() {
            return Err(Error::InvalidParam);
        }

        if self.backends.len() != usage.len() {
            return Err(Error::InvalidParam);
        }

        if self.backends.len() == 1 {
            self.backends[0].classify(desc, usage[0])
        } else {
            self.multi_classify(desc, usage)
        }
    }

    fn multi_classify(&self, desc: Description, usage: &[Usage]) -> Result<Class> {
        // call classify from all backends and merge the results
        let mut max_extent = Extent::max();
        let mut mods: Option<HashSet<Modifier>> = None;
        let mut con = Constraint::new();
        let mut required_idx = None;
        for (idx, (backend, &usage)) in self.backends.iter().zip(usage.iter()).enumerate() {
            if usage == Usage::Unused {
                continue;
            }

            let class = backend.classify(desc, usage)?;

            max_extent.intersect(class.max_extent, desc.is_buffer());

            if !desc.is_buffer() {
                let backend_mods: HashSet<Modifier> = class.modifiers.into_iter().collect();
                mods = Some(match mods {
                    Some(mods) => mods.intersection(&backend_mods).copied().collect(),
                    None => backend_mods,
                });
            }

            if let Some(backend_con) = class.constraint {
                con.merge(backend_con);
            } else if required_idx.is_none() {
                required_idx = Some(idx);
            } else {
                return Err(Error::NoSupport);
            }
        }

        if max_extent.is_empty(desc.is_buffer()) {
            return Err(Error::NoSupport);
        }

        let mods: Vec<Modifier> = if desc.is_buffer() {
            Vec::new()
        } else {
            let mods = mods.unwrap_or_default();
            if mods.is_empty() {
                return Err(Error::NoSupport);
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

    pub fn modifiers<'a>(&self, class: &'a Class) -> Option<&'a Vec<Modifier>> {
        // MOD_INVALID indicates an implicit modifier internally, but it means there is no modifier
        // support to users
        if class.modifiers.iter().any(|m| m.is_invalid()) {
            None
        } else {
            Some(&class.modifiers)
        }
    }

    pub(crate) fn backend(&self, idx: usize) -> &dyn Backend {
        self.backends[idx].as_ref()
    }
}

#[derive(Default)]
pub struct Builder {
    backends: Vec<Box<dyn super::Backend>>,
}

impl Builder {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn add_backend<T>(mut self, backend: T) -> Self
    where
        T: Backend + 'static,
    {
        self.backends.push(Box::new(backend));
        self
    }

    pub fn build(self) -> Result<Arc<Device>> {
        if self.backends.is_empty() {
            return Err(Error::InvalidParam);
        }

        let dev = Device {
            backends: self.backends,
        };

        Ok(Arc::new(dev))
    }
}
