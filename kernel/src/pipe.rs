//! A pipe: its two ends' holders and the bytes between them, behind its own
//! lock, which no copy and no frame allocation runs under
//! (`crate::frame_bytes`). A small read or write bounces through its stack in
//! one hold; a larger one claims its bytes. A second of either kind waits for
//! its turn (`frame_bytes::turn`).

use crate::frame_bytes::{self, Bounce, FrameBytes, BOUNCE};

/// A small read's or write's bytes, on its caller's stack.
pub type PipeBounce = Bounce<BOUNCE>;
use crate::mm::charge::{self, Charge, Exhausted};

use kernel::pipe::{End, Holders, Released};
use kernel::sched::task::WaitClass;

use alloc::sync::Arc;

use crate::scheduler::Parkable;
use crate::sync::Lock;
use crate::user_ptr::{Faulted, UserBytes, UserBytesMut};
use crate::watch::{Cancelled, Watch};

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
        state: Lock::new(State { held, holders: Holders::new(), rt_boost_pending: false }),
        read_watch: Arc::new(Watch::new()),
        write_watch: Arc::new(Watch::new()),
        left: [Arc::new(Watch::new()), Arc::new(Watch::new())],
        _charge: charge,
    }));
    Ok((PipeReader::acquire(&pipe), PipeWriter::acquire(&pipe)))
}

/// Bytes [`PipeRef::take_now`] took: the frames they left spare go back,
/// and a lent real-time window is taken, as it drops with every lock let go.
pub struct Taken {
    pub n: usize,
    _spares: frame_bytes::Spares,
    boost: bool,
}

impl Drop for Taken {
    fn drop(&mut self) {
        if self.boost {
            crate::scheduler::boost_current_rt_inherited();
        }
    }
}

pub enum PipeWrite {
    Wrote(usize),
    /// Not the room the write needs now: wait for `kernel::pipe::room`.
    NoRoom,
    BrokenPipe,
    /// The memory its bytes need could not be taken; no wait for room makes
    /// it appear, so a caller must not park on it.
    NoMemory,
    /// A byte of the writer's buffer it cannot have: nothing was written.
    Faulted(Faulted),
}

/// Why a read answered no count.
pub enum ReadStopped {
    /// A kill ended the wait for another read's claim.
    Cancelled,
    /// A byte of the reader's buffer it cannot have: the bytes it took are lost.
    Faulted(Faulted),
}

impl From<Cancelled> for ReadStopped {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl PipeRef {
    fn hold(&self, end: End) {
        self.0.state.lock().holders.hold(end);
    }

    /// Read what the pipe holds into `buf`, as much as fits: `None` while it
    /// holds nothing and a writer is left. Waits only for another read's
    /// claim to end, never for bytes.
    pub fn read(&self, p: &Parkable, buf: &mut UserBytesMut) -> Result<Option<usize>, ReadStopped> {
        let mut bounce = PipeBounce::new();
        let (n, runs, boost) = {
            let mut pipe = self.turn(p, End::Read)?;
            let available = pipe.held.len();
            if available == 0 {
                return Ok((!pipe.holders.held(End::Write)).then_some(0));
            }
            let n = buf.len().min(available);
            if n == 0 {
                return Ok(Some(0));
            }
            let boost = core::mem::take(&mut pipe.rt_boost_pending);
            if n <= BOUNCE {
                let spares = pipe.held.take(n, &mut bounce);
                drop(pipe);
                frame_bytes::give_back(&self.0.state, |s| &mut s.held, spares);
                if boost {
                    crate::scheduler::boost_current_rt_inherited();
                }
                buf.write_at(0, bounce.bytes()).map_err(ReadStopped::Faulted)?;
                return Ok(Some(n));
            }
            pipe.held.claim_read(n);
            (n, pipe.held.first_drain(0, n), boost)
        };
        if boost {
            // Outside the pipe's lock: the scheduler takes its own CPU-queue lock.
            crate::scheduler::boost_current_rt_inherited();
        }
        let copied = frame_bytes::drain(&self.0.state, |s| &mut s.held, 0, buf, n, runs);
        let (spares, ended, unread) = {
            let mut pipe = self.0.state.lock();
            let (spares, ended) = pipe.held.consume();
            (spares, ended, pipe.unread())
        };
        frame_bytes::give_back(&self.0.state, |s| &mut s.held, spares);
        drop(unread);
        frame_bytes::wake(&self.0.read_watch, ended);
        copied.map_err(ReadStopped::Faulted)?;
        Ok(Some(n))
    }

    /// Write `buf` as a pipe write lands (`kernel::pipe::takes`). Waits only
    /// for another write's claim to end, never for room.
    pub fn write(&self, p: &Parkable, buf: &UserBytes) -> Result<PipeWrite, Cancelled> {
        if buf.len() <= BOUNCE {
            let mut bounce = PipeBounce::new();
            return match bounce.read(buf) {
                Ok(src) => self.write_bounced(p, src),
                Err(faulted) => Ok(PipeWrite::Faulted(faulted)),
            };
        }
        let (n, first) = {
            let mut pipe = self.turn(p, End::Write)?;
            if !pipe.holders.held(End::Read) {
                return Ok(PipeWrite::BrokenPipe);
            }
            match kernel::pipe::takes(pipe.held.space(), buf.len()) {
                None => return Ok(PipeWrite::NoRoom),
                Some(0) => return Ok(PipeWrite::Wrote(0)),
                Some(n) => {
                    pipe.held.claim_write(n);
                    (n, pipe.held.first_fill(n))
                }
            }
        };
        let supplied = match first {
            frame_bytes::Fill::Runs(runs) => Ok(runs),
            frame_bytes::Fill::Supply(needs) => frame_bytes::supply(&self.0.state, |s| &mut s.held, needs, n),
        };
        let wrote = match supplied {
            Ok(runs) => match frame_bytes::fill(&self.0.state, |s| &mut s.held, buf, n, runs) {
                Ok(()) => PipeWrite::Wrote(n),
                Err(faulted) => PipeWrite::Faulted(faulted),
            },
            Err(Exhausted) => PipeWrite::NoMemory,
        };
        let (ended, unread) = {
            let mut pipe = self.0.state.lock();
            let ended = match wrote {
                PipeWrite::Wrote(_) => pipe.held.commit(),
                _ => pipe.held.abandon_write(),
            };
            (ended, pipe.unread())
        };
        drop(unread);
        frame_bytes::wake(&self.0.write_watch, ended);
        Ok(wrote)
    }

    /// [`Self::write`] of `src`, already copied in: whole in one hold, unless
    /// it needs frames, which it takes with the lock let go.
    fn write_bounced(&self, p: &Parkable, src: &PipeBounce) -> Result<PipeWrite, Cancelled> {
        let (n, needs) = {
            let mut pipe = self.turn(p, End::Write)?;
            if !pipe.holders.held(End::Read) {
                return Ok(PipeWrite::BrokenPipe);
            }
            let n = match kernel::pipe::takes(pipe.held.space(), src.bytes().len()) {
                None => return Ok(PipeWrite::NoRoom),
                Some(0) => return Ok(PipeWrite::Wrote(0)),
                Some(n) => n,
            };
            if pipe.held.put(&src.bytes()[..n]) {
                return Ok(PipeWrite::Wrote(n));
            }
            pipe.held.claim_write(n);
            match pipe.held.first_fill(n) {
                frame_bytes::Fill::Supply(needs) => (n, needs),
                frame_bytes::Fill::Runs(_) => unreachable!("a put refused wants frames"),
            }
        };
        let supplied = frame_bytes::supply(&self.0.state, |s| &mut s.held, needs, n);
        let (ended, unread) = {
            let mut pipe = self.0.state.lock();
            let ended = match supplied {
                Ok(_) => pipe.held.place_all(&src.bytes()[..n]),
                Err(Exhausted) => pipe.held.abandon_write(),
            };
            (ended, pipe.unread())
        };
        drop(unread);
        frame_bytes::wake(&self.0.write_watch, ended);
        Ok(match supplied {
            Ok(_) => PipeWrite::Wrote(n),
            Err(Exhausted) => PipeWrite::NoMemory,
        })
    }

    /// Write `src`, already copied in, whole in one hold of the pipe's lock,
    /// for a caller holding its handle-table lock: `None` where that cannot answer
    /// it without a wait or frames taken, which [`Self::write`] does.
    pub fn put_now(&self, src: &[u8]) -> Option<PipeWrite> {
        let mut pipe = self.0.state.lock();
        if pipe.held.claim_held(End::Write) {
            return None;
        }
        if !pipe.holders.held(End::Read) {
            return Some(PipeWrite::BrokenPipe);
        }
        match kernel::pipe::takes(pipe.held.space(), src.len())? {
            0 => Some(PipeWrite::Wrote(0)),
            n => pipe.held.put(&src[..n]).then_some(PipeWrite::Wrote(n)),
        }
    }

    /// Take at most `max` bytes, up to [`BOUNCE`], into `into` whole in one
    /// hold of the pipe's lock, for a caller holding its handle-table lock:
    /// `None` where the pipe holds nothing or more than a bounce would take,
    /// or another read holds its claim, which [`Self::read`] answers.
    pub fn take_now(&self, max: usize, into: &mut PipeBounce) -> Option<Taken> {
        let mut pipe = self.0.state.lock();
        let n = max.min(pipe.held.len());
        if n == 0 || n > BOUNCE || pipe.held.claim_held(End::Read) {
            return None;
        }
        let spares = pipe.held.take(n, into);
        Some(Taken { n, _spares: spares, boost: core::mem::take(&mut pipe.rt_boost_pending) })
    }

    /// The pipe's lock, held in `end`'s turn to claim.
    fn turn(&self, p: &Parkable, end: End) -> Result<crate::sync::LockGuard<'_, State>, Cancelled> {
        let watch = match end {
            End::Read => &self.0.read_watch,
            End::Write => &self.0.write_watch,
        };
        frame_bytes::turn(p, &self.0.state, |s| &mut s.held, end, watch, WaitClass::Pipe)
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
