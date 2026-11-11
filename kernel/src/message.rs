//! One direction of a connection: whole messages, each its bytes and the
//! handles it carries, from its senders to its receivers.
//!
//! A message is admitted whole under the queue's lock or refused with nothing
//! of it queued, and taken whole or left queued (`kernel::message`). Its
//! bytes are kept in frames counted to the job that made the connection
//! (`crate::frame_bytes`), its handles beside them in the same order.
//!
//! **No user copy runs under the queue's lock.** A message, at most
//! [`MAX_MESSAGE_LEN`] bytes, bounces through its caller's stack
//! ([`MessageBounce`]): a send copies it in with no lock held and queues it
//! whole in one hold ([`QueueRef::send_now`]), and a receive takes it whole
//! and installs its handles in one hold ([`QueueRef::recv_now`]), copying it
//! out after. A refusal takes nothing and queues nothing. A send whose bytes
//! want frames claims its room and takes them with the lock let go
//! ([`QueueRef::claim_send`]); one such claim at a time, and a send that meets
//! it waits for its turn (`frame_bytes::turn`).

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel::message::{Head, Queue, Untaken};
use kernel::pipe::{End, Holders, Released};
use kernel::sched::task::WaitClass;
use toyos_abi::handle::ReceivedEntry;
use toyos_abi::syscall::{MAX_MESSAGE_LEN, MAX_QUEUED_HANDLES, MAX_QUEUED_MESSAGES, MAX_TRANSFER_HANDLES};

use crate::frame_bytes::{self, Bounce, Fill, FrameBytes, Spares};
use crate::mm::charge::{self, Charge, Exhausted};
use crate::object::handle::{HandleEntry, HandleTable};
use crate::scheduler::Parkable;
use crate::sync::{Lock, LockGuard};
use crate::watch::{Cancelled, Watch};

/// A whole message's bytes, on its sender's or receiver's stack.
pub type MessageBounce = Bounce<MAX_MESSAGE_LEN>;

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

impl State {
    /// Every message nobody can take any more, to give back with the lock
    /// let go; kept while a claim's copy may still reach its bytes, whose end
    /// asks again.
    fn untaken(&mut self) -> Option<(FrameBytes, VecDeque<HandleEntry>)> {
        let owed = !self.holders.held(End::Read) && !self.bytes.claimed() && (self.bytes.frames() > 0 || !self.queue.is_empty());
        owed.then(|| {
            let job = Arc::clone(self.bytes.job());
            (core::mem::replace(&mut self.bytes, FrameBytes::new(&job)), self.queue.clear())
        })
    }

    /// What a claim's end, already made, leaves to do once the lock is let
    /// go: `spares` given back, what nobody can take, and its wake.
    fn ended(&mut self, ended: kernel::pipe::Ended, spares: Spares) -> Ended {
        Ended { spares, untaken: self.untaken(), ended, boost: false }
    }
}

/// [`State::ended`]'s answer.
struct Ended {
    spares: Spares,
    untaken: Option<(FrameBytes, VecDeque<HandleEntry>)>,
    ended: kernel::pipe::Ended,
    /// The real-time window a sender lent, taken with the message.
    boost: bool,
}

/// Why a message was not sent; nothing of it was.
pub enum SendRefused {
    /// No room for the whole message now: wait for [`QueueRef::fits`].
    NoRoom,
    /// No receiver is left.
    Gone,
    /// Its bytes' frames could not be taken; no wait makes them appear.
    NoMemory,
    /// A kill ended the wait for another send's claim.
    Cancelled,
}

/// Why [`QueueRef::send_now`] queued nothing.
pub enum Unsent {
    Refused(SendRefused),
    /// It cannot be made whole in this hold: another send's claim is held,
    /// or its bytes want frames. A claim sends it.
    Claim,
}

/// A message taken whole by [`QueueRef::recv_now`], its bytes in the
/// caller's bounce: what is left of the hold is done as it drops, with every
/// lock let go. Spares past its batch stay listed for the next take.
pub struct Received {
    pub head: Head,
    /// Each handle and its object's kind, read as it was installed.
    pub installed: [ReceivedEntry; MAX_TRANSFER_HANDLES],
    _spares: Spares,
    boost: bool,
}

impl Drop for Received {
    fn drop(&mut self) {
        if self.boost {
            crate::scheduler::boost_current_rt_inherited();
        }
    }
}

/// Why a receive claimed no message.
pub enum Unclaimed {
    /// Nothing queued, and a sender is left to send.
    Empty,
    /// Nothing queued, and no sender is left.
    Gone,
    /// The front message stays queued.
    Untaken(Untaken),
    /// The front message stays queued until the table grows by this many
    /// slots, which it may not under the queue's lock.
    Room(usize),
}

/// A send's room, claimed, with its frames taken: its bytes placed and the
/// message queued whole by [`Self::commit`], or dropped first, nothing of it.
pub struct SendClaim<'q> {
    queue: &'q QueueRef,
    src: &'q [u8],
    /// Set by the commit, which ends the claim.
    ended: Option<Ended>,
}

impl SendClaim<'_> {
    /// Queue the message with `batch`, its handles; a refusal hands the batch
    /// back and queues nothing.
    pub fn commit(&mut self, mut batch: Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, SendRefused)> {
        let mut state = self.queue.0.state.lock();
        if !state.holders.held(End::Read) {
            return Err((batch, SendRefused::Gone));
        }
        // It fits: the claim was made where it did, and only this claim adds.
        let ended = state.bytes.place_all(self.src);
        state.queue.push(self.src.len(), batch.drain(..));
        self.ended = Some(state.ended(ended, Spares::NONE));
        drop(state);
        // The empty batch's storage goes back with the lock let go.
        drop(batch);
        Ok(())
    }
}

impl Drop for SendClaim<'_> {
    fn drop(&mut self) {
        let ended = self.ended.take().unwrap_or_else(|| {
            let mut state = self.queue.0.state.lock();
            let ended = state.bytes.abandon_write();
            state.ended(ended, Spares::NONE)
        });
        self.queue.finish(End::Write, ended);
    }
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

    /// Queue `src`, already copied in, and `batch`, its handles, whole in
    /// this hold, called under the sender's handle-table lock as its handles move;
    /// a refusal hands the batch back and queues nothing. No copy waits on it.
    pub fn send_now(&self, src: &[u8], mut batch: Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, Unsent)> {
        let mut state = self.0.state.lock();
        if !state.bytes.turn(End::Write) {
            return Err((batch, Unsent::Claim));
        }
        if !state.holders.held(End::Read) {
            return Err((batch, Unsent::Refused(SendRefused::Gone)));
        }
        if !state.queue.fits(state.bytes.space(), src.len(), batch.len()) {
            return Err((batch, Unsent::Refused(SendRefused::NoRoom)));
        }
        if !state.bytes.put(src) {
            return Err((batch, Unsent::Claim));
        }
        state.queue.push(src.len(), batch.drain(..));
        drop(state);
        // The empty batch's storage goes back with the queue's lock let go.
        drop(batch);
        Ok(())
    }

    /// Take the front message whole into `into` and install its handles in
    /// `table`, in this hold, where a reader holding `buf` bytes and `cap`
    /// handles can take it, called under the receiver's handle-table lock. A table
    /// that must grow first answers [`Unclaimed::Room`], taking nothing.
    pub fn recv_now(&self, table: &mut HandleTable, buf: usize, cap: usize, into: &mut MessageBounce) -> Result<Received, Unclaimed> {
        let mut state = self.0.state.lock();
        let head = match state.queue.front(buf, cap) {
            Some(Ok(head)) => head,
            Some(Err(untaken)) => return Err(Unclaimed::Untaken(untaken)),
            None if state.holders.held(End::Write) => return Err(Unclaimed::Empty),
            None => return Err(Unclaimed::Gone),
        };
        if !table.has_room(head.handles) {
            return Err(Unclaimed::Room(head.handles));
        }
        let mut installed = [ReceivedEntry::NONE; MAX_TRANSFER_HANDLES];
        let Some(Ok((message, handles))) = state.queue.pop(head.len, head.handles, |_| true) else {
            unreachable!("message: a front its reader holds left the queue or was refused");
        };
        assert_eq!(message, head, "message: the front moved under its lock");
        for (slot, entry) in installed.iter_mut().zip(handles) {
            let kind = entry.object().object_kind() as u32;
            *slot = ReceivedEntry { handle: table.install(entry).expect("the table had room"), kind };
        }
        let spares = state.bytes.take(head.len, into);
        let boost = core::mem::take(&mut state.rt_boost_pending);
        Ok(Received { head, installed, _spares: spares, boost })
    }

    /// Claim room for `src`, a message's bytes, and its `handles`, and take
    /// the frames its bytes want with the lock let go: for a send
    /// [`Self::send_now`] could not make whole. Waits only for another send's
    /// claim to end, never for room.
    pub fn claim_send<'q>(&'q self, p: &Parkable, src: &'q [u8], handles: usize) -> Result<SendClaim<'q>, SendRefused> {
        let len = src.len();
        let first = {
            let mut state = self.turn(p, End::Write).map_err(|_| SendRefused::Cancelled)?;
            if !state.holders.held(End::Read) {
                return Err(SendRefused::Gone);
            }
            if !state.queue.fits(state.bytes.space(), len, handles) {
                return Err(SendRefused::NoRoom);
            }
            state.bytes.claim_write(len);
            state.bytes.first_fill(len)
        };
        let claim = SendClaim { queue: self, src, ended: None };
        if let Fill::Supply(needs) = first {
            frame_bytes::supply(&self.0.state, |s| &mut s.bytes, needs, len).map_err(|Exhausted| SendRefused::NoMemory)?;
        }
        Ok(claim)
    }

    /// The queue's lock, held in `end`'s turn to claim.
    fn turn(&self, p: &Parkable, end: End) -> Result<LockGuard<'_, State>, Cancelled> {
        frame_bytes::turn(p, &self.0.state, |s| &mut s.bytes, end, self.watch(end), WaitClass::Ipc)
    }

    fn watch(&self, end: End) -> &Watch {
        match end {
            End::Read => &self.0.read_watch,
            End::Write => &self.0.write_watch,
        }
    }

    /// What a claim's end left, done with the lock let go: frames go back to
    /// the allocator, a handle's release can run another object's hook, and
    /// the scheduler takes its own CPU-queue lock for a lent window.
    fn finish(&self, end: End, ended: Ended) {
        let Ended { spares, untaken, ended, boost } = ended;
        frame_bytes::give_back(&self.0.state, |s| &mut s.bytes, spares);
        drop(untaken);
        if boost {
            crate::scheduler::boost_current_rt_inherited();
        }
        frame_bytes::wake(self.watch(end), ended);
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
            (released, state.untaken())
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
