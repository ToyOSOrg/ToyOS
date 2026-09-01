//! The only place a process can resolve a service name. Immutable once
//! built — no insert, no remove, no replace — and a namespace narrowed
//! for a child cannot be widened back. There is no global registry: a
//! name a process was not given resolves to nothing.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::syscall::{MAX_NAMESPACE_ENTRIES, MAX_SERVICE_NAME};

use super::port::Connector;
use super::{KObjectVariant, ObjectCore};

pub struct Namespace {
    pub(super) core: ObjectCore,
    /// Sorted by name: lookup binary-searches this order.
    entries: Box<[(Box<str>, Arc<Connector>)]>,
}

/// Why a namespace could not be built.
pub enum BuildError {
    /// Too many entries, or a name past the length limit — refused, not truncated.
    TooMany,
    /// Two entries share a name.
    Duplicate,
}

impl Namespace {
    /// Entries must already carry every name this namespace is to hold.
    pub fn build(mut entries: Vec<(Box<str>, Arc<Connector>)>) -> Result<Arc<Self>, BuildError> {
        if entries.len() > MAX_NAMESPACE_ENTRIES {
            return Err(BuildError::TooMany);
        }
        if entries.iter().any(|(name, _)| name.len() > MAX_SERVICE_NAME) {
            return Err(BuildError::TooMany);
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if entries.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(BuildError::Duplicate);
        }
        Ok(Arc::new(Self {
            core: Self::new_core(),
            entries: entries.into_boxed_slice(),
        }))
    }

    pub fn lookup(&self, name: &str) -> Option<&Arc<Connector>> {
        let i = self.entries.binary_search_by(|(n, _)| (**n).cmp(name)).ok()?;
        Some(&self.entries[i].1)
    }

    /// Every binding, for a build carrying the whole of this one over.
    pub fn entries(&self) -> &[(Box<str>, Arc<Connector>)] {
        &self.entries
    }
}
