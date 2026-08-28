//! A connection: two pipes for the bytes, two handle queues for the handles.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::syscall::{SyscallError, MAX_QUEUED_BATCHES};

use crate::pipe::{PipeId, PipeReader, PipeWriter};
use crate::sync::Lock;

use super::handle::HandleEntry;
use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

/// Handles in flight in one direction of a connection; `None` once the reader is gone.
pub struct HandleQueue(Lock<Option<VecDeque<Vec<HandleEntry>>>>);

impl HandleQueue {
    fn open() -> Arc<Self> {
        Arc::new(Self(Lock::new(Some(VecDeque::new()))))
    }

    /// No reader and never one: what a joined connection's queues are.
    fn dead() -> Arc<Self> {
        Arc::new(Self(Lock::new(None)))
    }

    /// A refusal hands the batch back: dropped here, it would be capabilities destroyed silently.
    fn push(&self, batch: Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, SyscallError)> {
        let mut guard = self.0.lock();
        let Some(queue) = guard.as_mut() else { return Err((batch, SyscallError::Gone)) };
        if queue.len() >= MAX_QUEUED_BATCHES {
            return Err((batch, SyscallError::ResourceExhausted));
        }
        queue.push_back(batch);
        Ok(())
    }

    /// Refuses without taking when the batch is wider than `cap`, leaving it queued to retry.
    fn pop_bounded(&self, cap: usize) -> Result<Option<Vec<HandleEntry>>, SyscallError> {
        // One acquisition covers the check and the take: the batch measured is the batch returned.
        let mut guard = self.0.lock();
        let Some(queue) = guard.as_mut() else { return Ok(None) };
        match queue.front() {
            None => Ok(None),
            Some(batch) if batch.len() > cap => Err(SyscallError::InvalidArgument),
            Some(_) => Ok(queue.pop_front()),
        }
    }

    /// How wide the oldest batch is, without taking it; the front this reports is what `pop_bounded` takes.
    fn front_width(&self) -> Option<usize> {
        self.0.lock().as_ref()?.front().map(Vec::len)
    }

    /// Drops the batches outside the lock: releasing a handle can run another object's hook.
    pub(super) fn close_now(&self) {
        let batches = self.0.lock().take();
        drop(batches);
    }
}

/// One end of a connection: the queues are cross-wired, this end's `outbox` is the peer's `inbox`.
pub struct ConnectionEnd {
    pub(super) core: ObjectCore,
    rx: PipeId,
    tx: PipeId,
    inbox: Arc<HandleQueue>,
    outbox: Arc<HandleQueue>,
    reference: Held<(PipeReader, PipeWriter)>,
}

impl ConnectionEnd {
    /// One call for both ends, so the cross-wiring can't be gotten wrong from two constructors.
    pub fn pair_queues() -> (Arc<HandleQueue>, Arc<HandleQueue>) {
        (HandleQueue::open(), HandleQueue::open())
    }

    pub fn new(
        rx: PipeReader,
        tx: PipeWriter,
        inbox: Arc<HandleQueue>,
        outbox: Arc<HandleQueue>,
    ) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            rx: rx.id(),
            tx: tx.id(),
            inbox,
            outbox,
            reference: Held::new((rx, tx)),
        })
    }

    /// Two pipe ends that were never a port's, so both handle queues are dead.
    pub fn joined(rx: PipeReader, tx: PipeWriter) -> Arc<Self> {
        Self::new(rx, tx, HandleQueue::dead(), HandleQueue::dead())
    }

    pub fn rx(&self) -> PipeId {
        self.rx
    }

    pub fn tx(&self) -> PipeId {
        self.tx
    }

    /// See [`HandleQueue::push`]: a refusal comes back with the batch.
    pub fn send(
        &self,
        batch: Vec<HandleEntry>,
    ) -> Result<(), (Vec<HandleEntry>, SyscallError)> {
        self.outbox.push(batch)
    }

    pub fn recv_bounded(&self, cap: usize) -> Result<Option<Vec<HandleEntry>>, SyscallError> {
        self.inbox.pop_bounded(cap)
    }

    /// See [`HandleQueue::front_width`].
    pub fn peek_width(&self) -> Option<usize> {
        self.inbox.front_width()
    }
}

impl ZeroHandles for ConnectionEnd {
    fn on_zero_handles(&self) {
        // Only the inbox: what this end sent is still the peer's to receive.
        self.inbox.close_now();
        self.reference.release();
    }
}
