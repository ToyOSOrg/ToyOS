//! The handle-facing object for an inbox: what `SYS_INBOX_SETUP` installs, the
//! one owner of a [`crate::inbox::InboxRef`].

use alloc::sync::Arc;

use crate::inbox::{Inbox, InboxRef};
use crate::mm::charge::Exhausted;

use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

/// The ring's pages belong to the [`Inbox`]; this holds its one owning reference.
pub struct InboxObject {
    pub(super) core: ObjectCore,
    reference: Held<InboxRef>,
}

impl InboxObject {
    pub fn new(ring: InboxRef) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self {
            core: Self::new_core(0)?,
            reference: Held::new(ring),
        }))
    }

    /// The ring, for as long as a handle names it.
    pub fn inbox(&self) -> Option<Arc<Inbox>> {
        self.reference.with(InboxRef::inbox)
    }
}

impl ZeroHandles for InboxObject {
    fn on_zero_handles(&self) {
        self.reference.release();
    }
}
