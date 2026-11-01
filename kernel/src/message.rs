//! One direction of a connection: whole messages, each its bytes and the
//! handles it carries, from its senders to its receivers.
//!
//! A message is admitted whole under the queue's lock or refused with nothing
//! of it queued, and taken whole or left queued (`kernel::message`). Its
//! bytes are kept in frames counted to the job that made the connection
//! (`crate::frame_bytes`), its handles beside them in the same order.

use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel::message::{Head, Queue, Untaken};
use kernel::pipe::{End, Holders, Released};
use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::{MAX_QUEUED_HANDLES, MAX_QUEUED_MESSAGES, MAX_TRANSFER_HANDLES};

use crate::frame_bytes::FrameBytes;
use crate::mm::charge::{self, Charge, Exhausted};
use crate::object::handle::{HandleEntry, HandleTable};
use crate::sync::Lock;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::Watch;

/// A reference to a queue that holds neither end, as [`crate::pipe::PipeRef`]
/// is a pipe's.
#[derive(Clone)]
pub struct QueueRef(Arc<MessageQueue>);

pub use handle::{Receiver, Sender};

/// Owned references to a queue's two ends, each counted under the queue's
/// lock as it is made and let go as it drops.
mod handle {
    use super::{End, QueueRef};

    /// One counted reference to the receiving end.
    pub struct Receiver(QueueRef);

    /// One counted reference to the sending end.
    pub struct Sender(QueueRef);

    impl Receiver {
        pub(super) fn acquire(queue: &QueueRef) -> Self {
            queue.hold(End::Read);
            Self(queue.clone())
        }

        pub fn queue(&self) -> &QueueRef {
            &self.0
        }
    }

    impl Sender {
        pub(super) fn acquire(queue: &QueueRef) -> Self {
            queue.hold(End::Write);
            Self(queue.clone())
        }

        pub fn queue(&self) -> &QueueRef {
            &self.0
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            self.0.release(End::Read);
        }
    }

    impl Drop for Sender {
        fn drop(&mut self) {
            self.0.release(End::Write);
        }
    }
}

struct MessageQueue {
    state: Lock<State>,
    /// Posted by a send and by the last sender's leaving.
    read_watch: Arc<Watch>,
    /// Posted by a receive and by the last receiver's leaving.
    write_watch: Arc<Watch>,
    /// Itself, its watches and its queue at their bounds, charged to the job
    /// that made it, which its bytes are counted to as well.
    _charge: Charge,
}

struct State {
    bytes: FrameBytes,
    queue: Queue<HandleEntry>,
    holders: Holders,
    /// A real-time sender lent its window to whoever takes the next message,
    /// which reaches a receiver already runnable that the wake's boost misses.
    rt_boost_pending: bool,
}

/// Why a message was not sent; nothing of it was.
pub enum SendRefused {
    /// No room for the whole message now: wait for [`QueueRef::fits`].
    NoRoom,
    /// No receiver is left.
    Gone,
    /// Its bytes' frames could not be taken; no wait makes them appear.
    NoMemory,
}

/// What a receive answered.
pub enum Taken {
    /// A whole message, its bytes copied out and its handles installed at
    /// these numbers, the first `Head::handles` of them.
    Message(Head, [RawHandle; MAX_TRANSFER_HANDLES]),
    /// Nothing queued, and a sender is left to send.
    Empty,
    /// Nothing queued, and no sender is left.
    Gone,
    /// The front message stays queued.
    Untaken(Untaken),
}

/// Make a queue, answering its two ends.
pub fn create() -> Result<(Receiver, Sender), Exhausted> {
    let heap = charge::arc::<MessageQueue>()
        + 2 * charge::arc::<Watch>()
        // `VecDeque` doubles its storage, so each bound's next power of two.
        + charge::array::<Head>(MAX_QUEUED_MESSAGES.next_power_of_two())
        + charge::array::<HandleEntry>(MAX_QUEUED_HANDLES.next_power_of_two());
    let charge = Charge::current(heap)?;
    let queue = QueueRef(Arc::new(MessageQueue {
        state: Lock::new(State {
            bytes: FrameBytes::new(charge.job()),
            queue: Queue::new(),
            holders: Holders::new(),
            rt_boost_pending: false,
        }),
        read_watch: Arc::new(Watch::new()),
        write_watch: Arc::new(Watch::new()),
        _charge: charge,
    }));
    Ok((Receiver::acquire(&queue), Sender::acquire(&queue)))
}

impl QueueRef {
    fn hold(&self, end: End) {
        self.0.state.lock().holders.hold(end);
    }

    /// Queue `bytes` and `batch` as one message, or nothing of it. A refusal
    /// hands the batch back: dropped here, it would be capabilities destroyed
    /// silently.
    pub fn send(&self, bytes: &UserBytes, batch: Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, SendRefused)> {
        let mut state = self.0.state.lock();
        if !state.holders.held(End::Read) {
            return Err((batch, SendRefused::Gone));
        }
        if !state.queue.fits(state.bytes.space(), bytes.len(), batch.len()) {
            return Err((batch, SendRefused::NoRoom));
        }
        if state.bytes.write(bytes, bytes.len()).is_err() {
            return Err((batch, SendRefused::NoMemory));
        }
        state.queue.push(bytes.len(), batch.into_iter());
        Ok(())
    }

    /// Take the front message into `head` and then `body`, its first bytes
    /// filling `head`, and at most `cap` handles, each installed in `table`
    /// under this queue's lock.
    pub fn recv(&self, head: &mut UserBytesMut, body: &mut UserBytesMut, cap: usize, table: &mut HandleTable) -> Taken {
        let (taken, boost) = {
            let mut guard = self.0.state.lock();
            let State { bytes, queue, holders, rt_boost_pending } = &mut *guard;
            // Bound, not the block's tail: a tail match's temporaries outlive `guard`.
            let taken = match queue.pop(head.len() + body.len(), cap, |width| table.make_room(width)) {
                Some(Ok((message, handles))) => {
                    let mut installed = [RawHandle(0); MAX_TRANSFER_HANDLES];
                    for (slot, entry) in installed.iter_mut().zip(handles) {
                        *slot = table.install(entry).expect("room was made first");
                    }
                    let first = message.len.min(head.len());
                    bytes.read(head, first);
                    bytes.read(body, message.len - first);
                    (Taken::Message(message, installed), core::mem::take(rt_boost_pending))
                }
                Some(Err(untaken)) => (Taken::Untaken(untaken), false),
                None if holders.held(End::Write) => (Taken::Empty, false),
                None => (Taken::Gone, false),
            };
            taken
        };
        if boost {
            // Outside the queue's lock: the scheduler takes its own CPU-queue lock.
            crate::scheduler::boost_current_rt_inherited();
        }
        taken
    }

    /// A message to take, or no sender left: what a receive or a readable
    /// poll waits for.
    pub fn readable(&self) -> bool {
        let state = self.0.state.lock();
        !state.queue.is_empty() || !state.holders.held(End::Write)
    }

    /// Room for a message of `len` bytes and `handles`, or no receiver left:
    /// what a blocking send waits for.
    pub fn fits(&self, len: usize, handles: usize) -> bool {
        let state = self.0.state.lock();
        !state.holders.held(End::Read) || state.queue.fits(state.bytes.space(), len, handles)
    }

    /// Room for the largest message one send may make, or no receiver left:
    /// a writable poll, so a non-blocking send refused and retried on
    /// readiness is not refused for room again.
    pub fn writable(&self) -> bool {
        let state = self.0.state.lock();
        !state.holders.held(End::Read) || state.queue.fits_any(state.bytes.space())
    }

    /// Mark the queue so the next receive inherits RT priority.
    pub fn set_rt_boost_pending(&self) {
        self.0.state.lock().rt_boost_pending = true;
    }

    pub fn read_watch(&self) -> &Arc<Watch> {
        &self.0.read_watch
    }

    pub fn write_watch(&self) -> &Arc<Watch> {
        &self.0.write_watch
    }

    /// One reference to `end` lets go. The receivers' last gives back every
    /// message nobody can take any more and answers waiting senders; the
    /// senders' last answers waiting receivers.
    fn release(&self, end: End) {
        let (released, untaken) = {
            let mut state = self.0.state.lock();
            let released = state.holders.release(end);
            let untaken = (!state.holders.held(End::Read) && (state.bytes.frames() > 0 || !state.queue.is_empty())).then(|| {
                let job = Arc::clone(state.bytes.job());
                (core::mem::replace(&mut state.bytes, FrameBytes::new(&job)), state.queue.clear())
            });
            (released, untaken)
        };
        // Outside the lock: frames go back to the allocator, and a handle's
        // release can run another object's hook.
        drop(untaken);
        match (released, end) {
            (Released::Held | Released::PipeGone, _) => {}
            (Released::EndGone, End::Read) => self.0.write_watch.post(),
            (Released::EndGone, End::Write) => self.0.read_watch.post(),
        }
    }
}
