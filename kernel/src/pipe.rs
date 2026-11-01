//! A pipe: its two ends' holders and the bytes between them, behind its own
//! lock, which no copy and no frame allocation runs under
//! (`crate::frame_bytes`). A read and a write each claim their bytes; a second
//! of either kind waits, on its end's watch under [`TURN`], for the first's
//! claim to end.

use crate::frame_bytes::{self, FrameBytes};
use crate::mm::charge::{self, Charge, Exhausted};

use kernel::pipe::{End, Holders, Released};
use kernel::sched::task::WaitClass;

use alloc::sync::Arc;

use crate::scheduler::Parkable;
use crate::sync::Lock;
use crate::time::Deadline;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::{self, Cancelled, Watch};

/// A reference to a pipe that holds neither end: what an end's object, a
/// waiter and a poll keep to reach it. The pipe is its own: nothing else
/// names it, and its lock serializes nothing but its own traffic.
#[derive(Clone)]
pub struct PipeRef(Arc<Pipe>);

pub use handle::{PipeReader, PipeWriter};

/// Owned references to a pipe's two ends.
///
/// Each counts one holder of its end under the pipe's lock as it is made and
/// lets it go as it drops, so the counts are the live references and nothing
/// else.
mod handle {
    use super::{End, PipeRef};

    /// One counted reference to a pipe's read end — `Arc`, for a reader slot.
    pub struct PipeReader(PipeRef);

    /// One counted reference to a pipe's write end.
    pub struct PipeWriter(PipeRef);

    impl PipeReader {
        /// The only constructor: taking and counting the reference is one statement.
        pub(super) fn acquire(pipe: &PipeRef) -> Self {
            pipe.hold(End::Read);
            Self(pipe.clone())
        }

        pub fn pipe(&self) -> &PipeRef {
            &self.0
        }
    }

    impl PipeWriter {
        pub(super) fn acquire(pipe: &PipeRef) -> Self {
            pipe.hold(End::Write);
            Self(pipe.clone())
        }

        pub fn pipe(&self) -> &PipeRef {
            &self.0
        }
    }

    impl Clone for PipeReader {
        fn clone(&self) -> Self {
            Self::acquire(&self.0)
        }
    }

    impl Clone for PipeWriter {
        fn clone(&self) -> Self {
            Self::acquire(&self.0)
        }
    }

    impl Drop for PipeReader {
        fn drop(&mut self) {
            self.0.release(End::Read);
        }
    }

    impl Drop for PipeWriter {
        fn drop(&mut self) {
            self.0.release(End::Write);
        }
    }
}

struct Pipe {
    state: Lock<State>,
    /// The read end's and the write end's watches. Held by `Arc` so a blocking site or a
    /// poll registration can clone one out and hold it across its own park.
    read_watch: Arc<Watch>,
    write_watch: Arc<Watch>,
    /// Each end's leaving watch, by [`End`]: an `OTHER_END_GONE` poll on that
    /// end sits on it, and only the other end's last holder posts it — a byte
    /// moved through the pipe wakes no poll that asked about a leaving.
    left: [Arc<Watch>; 2],
    /// Its own heap and its watches, charged to the job that made it, which
    /// what it holds is counted to as well.
    _charge: Charge,
}

/// What a pipe's lock holds.
struct State {
    /// Given back once no reader is left to read it.
    held: FrameBytes,
    holders: Holders,
    /// Set when a write should hand the next reader transient RT priority — covers readers already runnable, which the wake-time boost misses.
    rt_boost_pending: bool,
    /// By [`slot`]: a copy found its end's claim held and waits for it to end.
    waiting: [bool; 2],
}

impl State {
    /// What no reader is left to read, to give back with the lock let go;
    /// kept while a claim's copy may still reach it, whose end asks again.
    fn unread(&mut self) -> Option<FrameBytes> {
        (!self.holders.held(End::Read) && !self.held.claimed() && self.held.frames() > 0).then(|| {
            let job = Arc::clone(self.held.job());
            core::mem::replace(&mut self.held, FrameBytes::new(&job))
        })
    }
}

/// The token a copy waiting for its end's claim waits under, on its end's
/// watch: a claim's end wakes it and no poll, and a post for bytes or room
/// wakes it to look again.
const TURN: u64 = 1;

impl Pipe {
    /// The heap one takes: itself and its four watches.
    fn heap() -> usize {
        charge::arc::<Pipe>() + 4 * charge::arc::<Watch>()
    }
}

/// Create a new pipe. Returns owned reader + writer references.
///
/// A pipe holds memory for its bytes only as it holds them: a write takes
/// what it needs, or is refused if none is left.
pub fn create() -> Result<(PipeReader, PipeWriter), Exhausted> {
    let charge = Charge::current(Pipe::heap())?;
    let held = FrameBytes::new(charge.job());
    let pipe = PipeRef(Arc::new(Pipe {
        state: Lock::new(State { held, holders: Holders::new(), rt_boost_pending: false, waiting: [false; 2] }),
        read_watch: Arc::new(Watch::new()),
        write_watch: Arc::new(Watch::new()),
        left: [Arc::new(Watch::new()), Arc::new(Watch::new())],
        _charge: charge,
    }));
    Ok((PipeReader::acquire(&pipe), PipeWriter::acquire(&pipe)))
}

pub enum PipeWrite {
    Wrote(usize),
    /// Not the room the write needs now: wait for `kernel::pipe::room`.
    NoRoom,
    BrokenPipe,
    /// The memory its bytes need could not be taken; no wait for room makes
    /// it appear, so a caller must not park on it.
    NoMemory,
}

impl PipeRef {
    fn hold(&self, end: End) {
        self.0.state.lock().holders.hold(end);
    }

    /// Read what the pipe holds into `buf`, as much as fits: `None` while it
    /// holds nothing and a writer is left. Waits only for another read's
    /// claim to end, never for bytes.
    pub fn read(&self, p: &Parkable, buf: &mut UserBytesMut) -> Result<Option<usize>, Cancelled> {
        let (n, runs, boost) = loop {
            let mut pipe = self.0.state.lock();
            let available = pipe.held.len();
            if available == 0 {
                return Ok((!pipe.holders.held(End::Write)).then_some(0));
            }
            let n = buf.len().min(available);
            if n == 0 {
                return Ok(Some(0));
            }
            if pipe.held.claim_read(n) {
                break (n, pipe.held.first_drain(0, n), core::mem::take(&mut pipe.rt_boost_pending));
            }
            pipe.waiting[slot(End::Read)] = true;
            drop(pipe);
            self.await_turn(p, End::Read)?;
        };
        if boost {
            // Outside the pipe's lock: the scheduler takes its own CPU-queue lock.
            crate::scheduler::boost_current_rt_inherited();
        }
        frame_bytes::drain(&self.0.state, |s| &mut s.held, 0, buf, n, runs);
        let (spares, unread, waiting) = {
            let mut pipe = self.0.state.lock();
            let spares = pipe.held.consume();
            (spares, pipe.unread(), core::mem::take(&mut pipe.waiting[slot(End::Read)]))
        };
        frame_bytes::give_back(&self.0.state, |s| &mut s.held, spares);
        drop(unread);
        self.turn_ended(End::Read, waiting);
        Ok(Some(n))
    }

    /// Write `buf` as a pipe write lands (`kernel::pipe::takes`). Waits only
    /// for another write's claim to end, never for room.
    pub fn write(&self, p: &Parkable, buf: &UserBytes) -> Result<PipeWrite, Cancelled> {
        let (n, first) = loop {
            let mut pipe = self.0.state.lock();
            if !pipe.holders.held(End::Read) {
                return Ok(PipeWrite::BrokenPipe);
            }
            if pipe.held.writing() {
                pipe.waiting[slot(End::Write)] = true;
                drop(pipe);
                self.await_turn(p, End::Write)?;
                continue;
            }
            match kernel::pipe::takes(pipe.held.space(), buf.len()) {
                None => return Ok(PipeWrite::NoRoom),
                Some(0) => return Ok(PipeWrite::Wrote(0)),
                Some(n) => {
                    assert!(pipe.held.claim_write(n), "pipe: a write claim refused with none held");
                    break (n, pipe.held.first_fill(n));
                }
            }
        };
        let supplied = match first {
            frame_bytes::Fill::Runs(runs) => Ok(runs),
            frame_bytes::Fill::Supply(needs) => frame_bytes::supply(&self.0.state, |s| &mut s.held, needs, n),
        };
        if let Ok(runs) = supplied {
            frame_bytes::fill(&self.0.state, |s| &mut s.held, buf, n, runs);
        }
        let (unread, waiting) = {
            let mut pipe = self.0.state.lock();
            match supplied {
                Ok(_) => pipe.held.commit(),
                Err(Exhausted) => pipe.held.abandon_write(),
            }
            (pipe.unread(), core::mem::take(&mut pipe.waiting[slot(End::Write)]))
        };
        drop(unread);
        self.turn_ended(End::Write, waiting);
        Ok(match supplied {
            Ok(_) => PipeWrite::Wrote(n),
            Err(Exhausted) => PipeWrite::NoMemory,
        })
    }

    /// Park until no copy holds `end`'s claim.
    fn await_turn(&self, p: &Parkable, end: End) -> Result<(), Cancelled> {
        let (watch, claimed): (&Watch, fn(&FrameBytes) -> bool) = match end {
            End::Read => (&self.0.read_watch, FrameBytes::reading),
            End::Write => (&self.0.write_watch, FrameBytes::writing),
        };
        watch::wait_until(p, watch, TURN, WaitClass::Pipe, Deadline::never(), || !claimed(&self.0.state.lock().held))
    }

    /// `end`'s claim ended: wake the copies that waited for it.
    fn turn_ended(&self, end: End, waiting: bool) {
        if waiting {
            let watch = match end {
                End::Read => &self.0.read_watch,
                End::Write => &self.0.write_watch,
            };
            watch.post_n(TURN, usize::MAX);
        }
    }

    pub fn has_data(&self) -> bool {
        let pipe = self.0.state.lock();
        pipe.holders.readable(pipe.held.len())
    }

    /// Whether a write waiting for `want` bytes of room answers now.
    pub fn has_space(&self, want: usize) -> bool {
        let pipe = self.0.state.lock();
        pipe.holders.writable(pipe.held.space(), want)
    }

    /// Whether the end opposite `end` of this pipe has no holder left.
    pub fn other_end_gone(&self, end: End) -> bool {
        self.0.state.lock().holders.other_end_gone(end)
    }

    /// Mark the pipe so the next consumer inherits RT priority.
    pub fn set_rt_boost_pending(&self) {
        self.0.state.lock().rt_boost_pending = true;
    }

    /// One reference to `end` lets go; its end's last posts the other end's
    /// watch and its leaving watch, and the read end's last gives back what
    /// the pipe holds, which nobody can read any more, or leaves it to the
    /// claim in flight to give back as it ends.
    fn release(&self, end: End) {
        let (released, unread) = {
            let mut pipe = self.0.state.lock();
            let released = pipe.holders.release(end);
            (released, pipe.unread())
        };
        // Outside the lock: its frames go back to the allocator.
        drop(unread);
        match (released, end) {
            (Released::Held | Released::PipeGone, _) => return,
            (Released::EndGone, End::Read) => crate::scheduler::wake_pipe_writers(self),
            (Released::EndGone, End::Write) => crate::scheduler::wake_pipe_readers(self),
        }
        self.0.left[slot(end.other())].post();
    }

    /// The read end's watch, for a blocking site, a post or a poll to clone and hold on its own stack.
    pub fn read_watch(&self) -> &Arc<Watch> {
        &self.0.read_watch
    }

    /// The write end's watch, the same way.
    pub fn write_watch(&self) -> &Arc<Watch> {
        &self.0.write_watch
    }

    /// `end`'s leaving watch, the same way: posted once, by the other end's
    /// last holder.
    pub fn left_watch(&self, end: End) -> &Arc<Watch> {
        &self.0.left[slot(end)]
    }
}

fn slot(end: End) -> usize {
    match end {
        End::Read => 0,
        End::Write => 1,
    }
}
