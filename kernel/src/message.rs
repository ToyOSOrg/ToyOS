//! One direction of a connection: whole messages, each its bytes and the
//! handles it carries, from its senders to its receivers.
//!
//! A message is admitted whole under the queue's lock or refused with nothing
//! of it queued, and taken whole or left queued (`kernel::message`). Its
//! bytes are kept in frames counted to the job that made the connection
//! (`crate::frame_bytes`), its handles beside them in the same order.
//!
//! **No copy runs under the queue's lock.** A send claims its room, copies its
//! bytes in and is queued by [`SendClaim::commit`]; a receive claims the front
//! message, copies its bytes out and takes it by [`RecvClaim::take`], which
//! installs its handles. Either claim dropped first leaves the queue as it
//! was, so a refusal anywhere takes nothing and queues nothing. One claim of
//! each kind at a time: another waits, on its end's watch under [`TURN`].

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel::message::{Head, Queue, Untaken};
use kernel::pipe::{End, Holders, Released};
use kernel::sched::task::WaitClass;
use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::{MAX_QUEUED_HANDLES, MAX_QUEUED_MESSAGES, MAX_TRANSFER_HANDLES};

use crate::frame_bytes::{self, Fill, FrameBytes, Runs, Spares};
use crate::mm::charge::{self, Charge, Exhausted};
use crate::object::handle::{HandleEntry, HandleTable};
use crate::scheduler::Parkable;
use crate::sync::Lock;
use crate::time::Deadline;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::{self, Cancelled, Watch};

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
    /// By [`slot`]: a send or a receive found its end's claim held and waits
    /// for it to end.
    waiting: [bool; 2],
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

    /// What the end of `end`'s claim, already made, leaves to do once the
    /// lock is let go: `spares` given back, what nobody can take, and a wake
    /// for the claims that waited.
    fn ended(&mut self, end: End, spares: Spares) -> Ended {
        Ended { spares, untaken: self.untaken(), waiting: core::mem::take(&mut self.waiting[slot(end)]) }
    }
}

/// [`State::ended`]'s answer.
struct Ended {
    spares: Spares,
    untaken: Option<(FrameBytes, VecDeque<HandleEntry>)>,
    waiting: bool,
}

/// The token a claim waits under for another of its kind to end, as a
/// pipe's does (`crate::pipe`).
const TURN: u64 = 1;

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

/// Why a receive claimed no message.
pub enum Unclaimed {
    /// Nothing queued, and a sender is left to send.
    Empty,
    /// Nothing queued, and no sender is left.
    Gone,
    /// The front message stays queued.
    Untaken(Untaken),
    /// A kill ended the wait for another receive's claim.
    Cancelled,
}

/// A send's room, claimed, with its bytes copied in: queued whole by
/// [`Self::commit`], or dropped first, nothing of it.
pub struct SendClaim<'q> {
    queue: &'q QueueRef,
    len: usize,
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
        state.queue.push(self.len, batch.drain(..));
        state.bytes.commit();
        self.ended = Some(state.ended(End::Write, Spares::NONE));
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
            state.bytes.abandon_write();
            state.ended(End::Write, Spares::NONE)
        });
        self.queue.finish(End::Write, ended);
    }
}

/// The front message, claimed: its bytes copied out by [`Self::copy`] and the
/// message taken by [`Self::take`], or dropped first and left queued.
pub struct RecvClaim<'q> {
    queue: &'q QueueRef,
    head: Head,
    /// Its first chunk's, which holds any message whole.
    runs: Runs,
    /// Set by the take, which ends the claim.
    ended: Option<Ended>,
}

impl RecvClaim<'_> {
    pub fn head(&self) -> Head {
        self.head
    }

    /// Copy the message's bytes into `head` and then `body`, its first bytes
    /// filling `head`; the two hold it whole.
    pub fn copy(&self, head: &mut UserBytesMut, body: &mut UserBytesMut) {
        let first = self.head.len.min(head.len());
        frame_bytes::drain(&self.queue.0.state, |s| &mut s.bytes, 0, head, first, self.runs);
        frame_bytes::drain(&self.queue.0.state, |s| &mut s.bytes, first, body, self.head.len - first, self.runs);
    }

    /// Take the message, its handles installed in `table` at the numbers
    /// answered, the first `Head::handles` of them; a table with no room for
    /// them leaves it queued whole.
    pub fn take(&mut self, table: &mut HandleTable) -> Result<[RawHandle; MAX_TRANSFER_HANDLES], Untaken> {
        // Room made before the queue's lock: it may grow the table.
        let room = self.head.handles == 0 || table.make_room(self.head.handles);
        let mut state = self.queue.0.state.lock();
        let (message, handles) = state
            .queue
            .pop(self.head.len, self.head.handles, |_| room)
            .expect("message: a claimed message left the queue")?;
        assert_eq!(message, self.head, "message: the front moved under its claim");
        let mut installed = [RawHandle(0); MAX_TRANSFER_HANDLES];
        for (slot, entry) in installed.iter_mut().zip(handles) {
            *slot = table.install(entry).expect("room was made first");
        }
        let spares = state.bytes.consume();
        self.ended = Some(state.ended(End::Read, spares));
        Ok(installed)
    }
}

impl Drop for RecvClaim<'_> {
    fn drop(&mut self) {
        let ended = self.ended.take().unwrap_or_else(|| {
            let mut state = self.queue.0.state.lock();
            state.bytes.abandon_read();
            state.ended(End::Read, Spares::NONE)
        });
        self.queue.finish(End::Read, ended);
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
            waiting: [false; 2],
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

    /// Claim room for a message of `bytes` and `handles` and copy its bytes
    /// in, with no lock held across the copy. Waits only for another send's
    /// claim to end, never for room.
    pub fn claim_send(&self, p: &Parkable, bytes: &UserBytes, handles: usize) -> Result<SendClaim<'_>, SendRefused> {
        let len = bytes.len();
        let first = loop {
            let mut state = self.0.state.lock();
            if !state.holders.held(End::Read) {
                return Err(SendRefused::Gone);
            }
            if state.bytes.writing() {
                state.waiting[slot(End::Write)] = true;
                drop(state);
                self.await_turn(p, End::Write).map_err(|_| SendRefused::Cancelled)?;
                continue;
            }
            if !state.queue.fits(state.bytes.space(), len, handles) {
                return Err(SendRefused::NoRoom);
            }
            assert!(state.bytes.claim_write(len), "message: a send claim refused with none held");
            break state.bytes.first_fill(len);
        };
        let claim = SendClaim { queue: self, len, ended: None };
        let runs = match first {
            Fill::Runs(runs) => runs,
            Fill::Supply(needs) => {
                frame_bytes::supply(&self.0.state, |s| &mut s.bytes, needs, len).map_err(|Exhausted| SendRefused::NoMemory)?
            }
        };
        frame_bytes::fill(&self.0.state, |s| &mut s.bytes, bytes, len, runs);
        Ok(claim)
    }

    /// Claim the front message where a reader holding `buf` bytes and `cap`
    /// handles can take it whole. Waits only for another receive's claim to
    /// end, never for a message.
    pub fn claim_recv(&self, p: &Parkable, buf: usize, cap: usize) -> Result<RecvClaim<'_>, Unclaimed> {
        let (claim, boost) = loop {
            let mut state = self.0.state.lock();
            if state.bytes.reading() {
                state.waiting[slot(End::Read)] = true;
                drop(state);
                self.await_turn(p, End::Read).map_err(|_| Unclaimed::Cancelled)?;
                continue;
            }
            let head = match state.queue.front(buf, cap) {
                Some(Ok(head)) => head,
                Some(Err(untaken)) => return Err(Unclaimed::Untaken(untaken)),
                None if state.holders.held(End::Write) => return Err(Unclaimed::Empty),
                None => return Err(Unclaimed::Gone),
            };
            assert!(state.bytes.claim_read(head.len), "message: a receive claim refused with none held");
            let runs = state.bytes.first_drain(0, head.len);
            break (RecvClaim { queue: self, head, runs, ended: None }, core::mem::take(&mut state.rt_boost_pending));
        };
        if boost {
            // Outside the queue's lock: the scheduler takes its own CPU-queue lock.
            crate::scheduler::boost_current_rt_inherited();
        }
        Ok(claim)
    }

    /// Park until no copy holds `end`'s claim.
    fn await_turn(&self, p: &Parkable, end: End) -> Result<(), Cancelled> {
        let (watch, claimed): (&Watch, fn(&FrameBytes) -> bool) = match end {
            End::Read => (&self.0.read_watch, FrameBytes::reading),
            End::Write => (&self.0.write_watch, FrameBytes::writing),
        };
        watch::wait_until(p, watch, TURN, WaitClass::Ipc, Deadline::never(), || !claimed(&self.0.state.lock().bytes))
    }

    /// What a claim's end left, done with the lock let go: frames go back to
    /// the allocator, and a handle's release can run another object's hook.
    fn finish(&self, end: End, ended: Ended) {
        let Ended { spares, untaken, waiting } = ended;
        frame_bytes::give_back(&self.0.state, |s| &mut s.bytes, spares);
        drop(untaken);
        self.turn_ended(end, waiting);
    }

    /// `end`'s claim ended: wake the claims that waited for it.
    fn turn_ended(&self, end: End, waiting: bool) {
        if waiting {
            let watch = match end {
                End::Read => &self.0.read_watch,
                End::Write => &self.0.write_watch,
            };
            watch.post_n(TURN, usize::MAX);
        }
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

fn slot(end: End) -> usize {
    match end {
        End::Read => 0,
        End::Write => 1,
    }
}
