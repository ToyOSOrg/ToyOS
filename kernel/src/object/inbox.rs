//! The handle-facing object for an inbox.
//!
//! Distinct from [`completion::inbox`](crate::completion::inbox)'s `Inbox`: that is a
//! task's bounded record ring, never handle-named; this is what `SYS_INBOX_SETUP`
//! installs, a counted reference to [`crate::inbox::Inbox`].

use alloc::sync::Arc;

use crate::inbox::InboxRef;

use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

/// The ring's pages belong to the instance keyed by [`InboxId`](crate::inbox::InboxId); this holds only the counted reference.
pub struct InboxObject {
    pub(super) core: ObjectCore,
    id: crate::inbox::InboxId,
    reference: Held<InboxRef>,
}

impl InboxObject {
    pub fn new(ring: InboxRef) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            id: ring.id(),
            reference: Held::new(ring),
        })
    }

    pub fn id(&self) -> crate::inbox::InboxId {
        self.id
    }
}

impl ZeroHandles for InboxObject {
    fn on_zero_handles(&self) {
        self.reference.release();
    }
}
