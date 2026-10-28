//! A connection: two pipes for the bytes, two handle queues for the handles.

use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel::message::{Batches, Refused, Window};
use kernel::pipe::Landing;
use toyos_abi::syscall::{SyscallError, MAX_QUEUED_BATCHES, MAX_TRANSFER_HANDLES};

use crate::mm::charge::{self, Charge, Exhausted};
use crate::pipe::{AtBatch, PipeReader, PipeRef, PipeWrite, PipeWriter};
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::sync::Lock;

use super::handle::HandleEntry;
use super::port::{PendingConnection, Stamp};
use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

/// Handles in flight in one direction of a connection, each batch pinned at
/// the stream position it follows (`kernel::message`); `None` once the reader
/// is gone.
///
/// **Its lock is taken before the pipe's**, by the sender for the whole of a
/// message and by the reader for a read or a receive, so a reader that has
/// read a message's bytes finds its batch pinned.
///
/// Charged at its bound when it is made, so that no send has a heap
/// allocation of its own to be refused.
pub struct HandleQueue {
    batches: Lock<Option<Batches<Vec<HandleEntry>>>>,
    _charge: Charge,
}

/// Why a message was not sent; nothing of it was.
pub enum SendRefused {
    /// No room for the whole message now: wait on the write watch.
    NoRoom,
    Error(SyscallError),
}

impl HandleQueue {
    fn open() -> Result<Arc<Self>, Exhausted> {
        // `VecDeque` doubles its storage, so the bound's next power of two.
        let bound = charge::arc::<Self>()
            + charge::array::<(u64, Vec<HandleEntry>)>(MAX_QUEUED_BATCHES.next_power_of_two())
            + MAX_QUEUED_BATCHES * charge::array::<HandleEntry>(MAX_TRANSFER_HANDLES);
        let charge = Charge::current(bound)?;
        Ok(Arc::new(Self { batches: Lock::new(Some(Batches::new())), _charge: charge }))
    }

    /// Write `bytes` to `tx` and pin `batch` just past them, whole or not at
    /// all. A refusal hands the batch back: dropped here, it would be
    /// capabilities destroyed silently.
    fn send(&self, tx: &PipeRef, bytes: &UserBytes, batch: Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, SendRefused)> {
        let mut guard = self.batches.lock();
        let Some(batches) = guard.as_mut() else { return Err((batch, SendRefused::Error(SyscallError::Gone))) };
        let carries = !batch.is_empty();
        match batches.admit(bytes.len(), carries, MAX_QUEUED_BATCHES) {
            Ok(()) => {}
            Err(Refused::Empty) => return Err((batch, SendRefused::Error(SyscallError::InvalidArgument))),
            Err(Refused::QueueFull) => return Err((batch, SendRefused::Error(SyscallError::ResourceExhausted))),
        }
        match tx.write(bytes, Landing::Whole) {
            PipeWrite::Wrote { end, .. } => {
                if carries {
                    batches.pin(end, batch);
                }
                Ok(())
            }
            PipeWrite::NoRoom => Err((batch, SendRefused::NoRoom)),
            PipeWrite::BrokenPipe => Err((batch, SendRefused::Error(SyscallError::Gone))),
            PipeWrite::NoMemory => Err((batch, SendRefused::Error(SyscallError::ResourceExhausted))),
        }
    }

    /// A read of `rx`, stopping at the next pinned batch.
    fn read(&self, rx: &PipeRef, buf: &mut UserBytesMut) -> Option<Result<usize, AtBatch>> {
        let guard = self.batches.lock();
        rx.try_read_within(buf, |read, available| match guard.as_ref() {
            Some(batches) => batches.window(read, available),
            None => Window::Bytes(available),
        })
    }

    /// Take the batch pinned at `rx`'s read position, refusing without taking
    /// when it is wider than `cap`, and leaving it pinned when `room` refuses.
    fn take(&self, rx: &PipeRef, cap: usize, room: impl FnOnce(usize) -> bool) -> Result<Option<Vec<HandleEntry>>, SyscallError> {
        let mut guard = self.batches.lock();
        let Some(batches) = guard.as_mut() else { return Ok(None) };
        let read = rx.read_position();
        let Some(width) = batches.at(read).map(Vec::len) else { return Ok(None) };
        if width > cap {
            return Err(SyscallError::InvalidArgument);
        }
        if !room(width) {
            return Err(SyscallError::ResourceExhausted);
        }
        Ok(batches.take(read))
    }

    /// Whether a batch is pinned where the reader is, which is something to receive.
    fn pinned_here(&self, rx: &PipeRef) -> bool {
        let guard = self.batches.lock();
        let Some(batches) = guard.as_ref() else { return false };
        batches.at(rx.read_position()).is_some()
    }

    /// Drops the batches outside the lock: releasing a handle can run another object's hook.
    pub(super) fn close_now(&self) {
        let batches = self.batches.lock().take();
        drop(batches);
    }
}

/// One end of a connection: the queues are cross-wired, this end's `outbox` is the peer's `inbox`.
pub struct ConnectionEnd {
    pub(super) core: ObjectCore,
    rx: PipeRef,
    tx: PipeRef,
    inbox: Arc<HandleQueue>,
    outbox: Arc<HandleQueue>,
    reference: Held<(PipeReader, PipeWriter)>,
    /// The server's end of a port's connection; `None` on a client's end.
    stamp: Option<Stamp>,
}

impl ConnectionEnd {
    /// One call for both ends, so the cross-wiring can't be gotten wrong from two constructors.
    pub fn pair_queues() -> Result<(Arc<HandleQueue>, Arc<HandleQueue>), Exhausted> {
        Ok((HandleQueue::open()?, HandleQueue::open()?))
    }

    pub fn new(
        rx: PipeReader,
        tx: PipeWriter,
        inbox: Arc<HandleQueue>,
        outbox: Arc<HandleQueue>,
    ) -> Result<Arc<Self>, Exhausted> {
        Ok(Self::build(Self::new_core(0)?, rx, tx, inbox, outbox, None))
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
        Self::build(core, conn.rx, conn.tx, conn.inbox, conn.outbox, Some(conn.stamp))
    }

    fn build(
        core: ObjectCore,
        rx: PipeReader,
        tx: PipeWriter,
        inbox: Arc<HandleQueue>,
        outbox: Arc<HandleQueue>,
        stamp: Option<Stamp>,
    ) -> Arc<Self> {
        Arc::new(Self {
            core,
            rx: rx.pipe().clone(),
            tx: tx.pipe().clone(),
            inbox,
            outbox,
            reference: Held::new((rx, tx)),
            stamp,
        })
    }

    pub fn stamp(&self) -> Option<&Stamp> {
        self.stamp.as_ref()
    }

    pub fn rx(&self) -> &PipeRef {
        &self.rx
    }

    pub fn tx(&self) -> &PipeRef {
        &self.tx
    }

    /// See [`HandleQueue::send`]: a refusal comes back with the batch.
    pub fn send(&self, bytes: &UserBytes, batch: Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, SendRefused)> {
        self.outbox.send(&self.tx, bytes, batch)
    }

    /// See [`HandleQueue::read`].
    pub fn read(&self, buf: &mut UserBytesMut) -> Option<Result<usize, AtBatch>> {
        self.inbox.read(&self.rx, buf)
    }

    /// See [`HandleQueue::take`].
    pub fn recv(&self, cap: usize, room: impl FnOnce(usize) -> bool) -> Result<Option<Vec<HandleEntry>>, SyscallError> {
        self.inbox.take(&self.rx, cap, room)
    }

    /// Bytes to read, the peer's end, or a batch pinned where the reader is.
    pub fn has_data(&self) -> bool {
        self.rx.has_data() || self.inbox.pinned_here(&self.rx)
    }
}

impl ZeroHandles for ConnectionEnd {
    fn on_zero_handles(&self) {
        // Only the inbox: what this end sent is still the peer's to receive.
        self.inbox.close_now();
        self.reference.release();
    }
}
