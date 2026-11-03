//! A handle to the device set (`toyos_abi::devset`): minted from a `SysCap`
//! carrying `DEVICE`, and carrying `READ` and `WAIT` alone, so its holder can
//! neither duplicate it nor hand it on.
//!
//! **Level-triggered**: it is readable while the set's generation differs from
//! the one its last read answered, so a reader that waits on it after a read
//! misses no change.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{KObjectVariant, ObjectCore};
use crate::mm::charge::Exhausted;

pub struct DeviceSet {
    pub(super) core: ObjectCore,
    /// The generation its last read answered; 0 before the first.
    seen: AtomicU64,
}

impl DeviceSet {
    pub fn new() -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self { core: Self::new_core(0)?, seen: AtomicU64::new(0) }))
    }

    /// Whether the set changed since this handle last read it.
    pub fn changed(&self) -> bool {
        crate::pcidev::generation() != self.seen.load(Ordering::Acquire)
    }

    /// Mark `generation` as read through this handle.
    pub fn saw(&self, generation: u64) {
        self.seen.store(generation, Ordering::Release);
    }
}
