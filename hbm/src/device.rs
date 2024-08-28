// Copyright 2024 Google LLC
// SPDX-License-Identifier: MIT

use super::backends::{Backend, Class, Constraint, Description, Extent, Usage};
use super::types::{Error, Format, Modifier, Result};
use std::collections::HashSet;
use std::sync::Arc;

pub struct Device {
    backends: Vec<Box<dyn Backend>>,
}

impl Device {
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

    pub fn modifiers<'a>(&self, class: &'a Class) -> &'a Vec<Modifier> {
        static EMPTY: Vec<Modifier> = Vec::new();

        // MOD_INVALID indicates an implicit modifier internally, but it means there is no modifier
        // support to users
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
            return Error::user();
        }

        let dev = Device {
            backends: self.backends,
        };

        Ok(Arc::new(dev))
    }
}
