//! The one object whose whole authority is in the rights on the handle.
//!
//! Minting a device claim, entering the RT band, and turning a pid into a
//! process handle are each reachable only through one bit on a handle to
//! this. The kernel creates exactly one full-rights `SysCap`, at boot, for
//! `/bin/init`; nothing else can construct one, so the set of processes
//! that can ever do the three is exactly what init endowed.

use alloc::sync::Arc;

use super::{KObjectVariant, ObjectCore};

pub struct SysCap {
    /// Visible to `object`, where `kobject!` generates this type's `core()`.
    pub(super) core: ObjectCore,
}

impl SysCap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { core: Self::new_core() })
    }
}
