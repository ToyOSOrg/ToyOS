//! The null object: every read answers end of file, every write takes all it
//! is given, and both directions are always ready. It names nothing, so it
//! carries no authority and holds no state; `SYS_NULL` mints one per call.

use alloc::sync::Arc;

use super::{KObjectVariant, ObjectCore};

pub struct NullObject {
    /// Visible to `object`, where `kobject!` generates this type's `core()`.
    pub(super) core: ObjectCore,
}

impl NullObject {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { core: Self::new_core() })
    }
}
