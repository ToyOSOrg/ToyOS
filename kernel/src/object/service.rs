//! A connection: two message queues, one per direction (`crate::message`).

use alloc::sync::Arc;

use crate::message::{QueueRef, Receiver, Sender};
use crate::mm::charge::{self, Exhausted};

use super::port::{PendingConnection, Stamp};
use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

/// One end of a connection: it receives on `rx` and sends on `tx`, which are
/// the peer's `tx` and `rx`.
pub struct ConnectionEnd {
    pub(super) core: ObjectCore,
    rx: QueueRef,
    tx: QueueRef,
    reference: Held<(Receiver, Sender)>,
    /// The server's end of a port's connection; `None` on a client's end.
    stamp: Option<Stamp>,
}

impl ConnectionEnd {
    pub fn new(rx: Receiver, tx: Sender) -> Result<Arc<Self>, Exhausted> {
        Ok(Self::build(Self::new_core(0)?, rx, tx, None))
    }

    /// What a server's end is charged, a stamp's badge at its bound
    /// included: taken before a connection leaves its port's queue, so a
    /// refused accept leaves it there.
    pub fn accept_core() -> Result<ObjectCore, Exhausted> {
        let badge = core::alloc::Layout::new::<[usize; 2]>()
            .extend(core::alloc::Layout::array::<u8>(toyos_abi::syscall::MAX_BADGE).expect("service: a badge's layout"))
            .expect("service: a badge's layout")
            .0;
        Self::new_core(charge::heap(badge.pad_to_align()))
    }

    /// The server's end of a queued connection, over what
    /// [`accept_core`](Self::accept_core) charged, keeping its stamp.
    pub fn accepted(core: ObjectCore, conn: PendingConnection) -> Arc<Self> {
        Self::build(core, conn.rx, conn.tx, Some(conn.stamp))
    }

    fn build(core: ObjectCore, rx: Receiver, tx: Sender, stamp: Option<Stamp>) -> Arc<Self> {
        Arc::new(Self { core, rx: rx.queue().clone(), tx: tx.queue().clone(), reference: Held::new((rx, tx)), stamp })
    }

    pub fn stamp(&self) -> Option<&Stamp> {
        self.stamp.as_ref()
    }

    /// What this end receives.
    pub fn rx(&self) -> &QueueRef {
        &self.rx
    }

    /// What this end sends to.
    pub fn tx(&self) -> &QueueRef {
        &self.tx
    }
}

impl ZeroHandles for ConnectionEnd {
    fn on_zero_handles(&self) {
        // The receiver's leaving gives back what was sent to this end; what it
        // sent stays the peer's to receive.
        self.reference.release();
    }
}
