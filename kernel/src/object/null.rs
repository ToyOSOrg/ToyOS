//! The null object: every read answers end of file, every write takes all it
//! is given, and both directions are always ready. It names nothing, so it
//! carries no authority and holds no state; `SYS_NULL` mints one per call.

use alloc::sync::Arc;

use toyos_abi::handle::Rights;

use crate::mm::charge::Exhausted;

use super::{KObjectVariant, ObjectCore};

pub struct NullObject {
    pub(super) core: ObjectCore,
}

impl NullObject {
    /// What a fresh null handle may carry.
    pub const RIGHTS: Rights = Rights::DUP
        .union(Rights::TRANSFER)
        .union(Rights::WAIT)
        .union(Rights::READ)
        .union(Rights::WRITE);

    pub fn new() -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self { core: Self::new_core(0)? }))
    }
}
