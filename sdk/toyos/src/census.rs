//! The kernel's live-object count, per kind.
//!
//! Two readings and a comparison, never one reading against a constant: an
//! object released by another process is dropped from the deferred queue on
//! whichever CPU drains next, so a single sample can be high by whatever has
//! not drained yet. A leak is not a lag — it accumulates.

use core::fmt;

use toyos_abi::syscall::debug_action::CENSUS_KIND;
use toyos_abi::handle::ObjectKind;
use toyos_abi::syscall;

/// How many objects of every kind are alive right now.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Census([u64; ObjectKind::ALL.len()]);

impl Census {
    pub fn now() -> Self {
        let mut counts = [0u64; ObjectKind::ALL.len()];
        for (slot, kind) in counts.iter_mut().zip(ObjectKind::ALL) {
            *slot = syscall::debug_with(CENSUS_KIND, kind as u64);
        }
        Self(counts)
    }

    /// Live objects of one kind.
    pub fn kind(&self, kind: ObjectKind) -> u64 {
        self.0[kind as usize]
    }

    pub fn total(&self) -> u64 {
        self.0.iter().sum()
    }

    /// Every kind this reading holds more of than `before`, with both counts.
    pub fn grown_since(
        &self,
        before: &Census,
    ) -> impl Iterator<Item = (ObjectKind, u64, u64)> + '_ {
        let earlier = before.0;
        ObjectKind::ALL
            .iter()
            .enumerate()
            .filter(move |(i, _)| self.0[*i] > earlier[*i])
            .map(move |(i, kind)| (*kind, earlier[i], self.0[i]))
    }
}

impl fmt::Display for Census {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (kind, live) in ObjectKind::ALL.iter().zip(self.0.iter()).filter(|(_, n)| **n > 0) {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{kind:?} {live}")?;
            first = false;
        }
        if first {
            write!(f, "nothing")?;
        }
        Ok(())
    }
}
