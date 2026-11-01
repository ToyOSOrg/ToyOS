//! A hold on how long any idle CPU takes to wake: from its making until it
//! goes, no CPU enters a state slower to leave than the least bound held, and
//! its making wakes each CPU waiting in one (`crate::idle::hold`). It is
//! made through a `SysCap` carrying `WAKE_LATENCY` and lasts as long as the
//! object, so its last handle's going, the holder's death among them, ends it.

use alloc::sync::Arc;

use toyos_abi::handle::Rights;

use crate::mm::charge::Exhausted;

use super::{KObjectVariant, ObjectCore};

pub struct WakeLatencyObject {
    pub(super) core: ObjectCore,
    bound_ns: u64,
}

impl WakeLatencyObject {
    /// It is held, not used: a handle may be passed on and duplicated, and
    /// does nothing else.
    pub const RIGHTS: Rights = Rights::DUP.union(Rights::TRANSFER);

    pub fn new(bound_ns: u64) -> Result<Arc<Self>, Exhausted> {
        let object = Arc::new(Self { core: Self::new_core(0)?, bound_ns });
        crate::idle::hold(bound_ns);
        Ok(object)
    }
}

impl Drop for WakeLatencyObject {
    fn drop(&mut self) {
        crate::idle::release(self.bound_ns);
    }
}
