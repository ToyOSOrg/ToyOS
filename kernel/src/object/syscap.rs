//! The one object whose whole authority is in the rights on the handle.
//!
//! Minting a device claim, entering the RT band, and turning a pid into a
//! process handle are each reachable only through one bit on a handle to
//! this. The kernel creates exactly one full-rights `SysCap`, at boot, for
//! the supervisor; nothing else can construct one, so the set of processes
//! that can ever do the three is exactly what the supervisor endowed.

use alloc::sync::Arc;

use crate::mm::charge::Exhausted;

use super::{KObjectVariant, ObjectCore};

pub struct SysCap {
    /// Visible to `object`, where `kobject!` generates this type's `core()`.
    pub(super) core: ObjectCore,
}

impl SysCap {
    pub fn new() -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self { core: Self::new_core(0)? }))
    }
}
