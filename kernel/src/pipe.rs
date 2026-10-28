use crate::frame_bytes::FrameBytes;
use crate::mm::charge::{self, Charge, Exhausted};

use kernel::pipe::{End, Holders, Released};

use alloc::sync::Arc;

use crate::watch::Watch;
use crate::sync::Lock;
use crate::user_ptr::{UserBytes, UserBytesMut};

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

    pub fn try_read(&self, buf: &mut UserBytesMut) -> Option<usize> {
        let (result, boost) = {
            let mut pipe = self.0.state.lock();
            let available = pipe.held.len();
            if available > 0 {
                let n = buf.len().min(available);
                pipe.held.read(buf, n);
                (Some(n), core::mem::take(&mut pipe.rt_boost_pending))
            } else if !pipe.holders.held(End::Write) && available == 0 {
                (Some(0), false)
            } else {
                (None, false)
            }
        };
        if boost {
            // Outside the pipe's lock: the scheduler takes its own CPU-queue lock.
            crate::scheduler::boost_current_rt_inherited();
        }
        result
    }

    /// Write `buf` as a pipe write lands (`kernel::pipe::takes`), under the
    /// one lock hold.
    pub fn write(&self, buf: &UserBytes) -> PipeWrite {
        let mut pipe = self.0.state.lock();
        if !pipe.holders.held(End::Read) {
            return PipeWrite::BrokenPipe;
        }
        let Some(n) = kernel::pipe::takes(pipe.held.space(), buf.len()) else { return PipeWrite::NoRoom };
        if pipe.held.write(buf, n).is_err() {
            return PipeWrite::NoMemory;
        }
        PipeWrite::Wrote(n)
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
    /// the pipe holds, which nobody can read any more.
    fn release(&self, end: End) {
        let (released, unread) = {
            let mut pipe = self.0.state.lock();
            let released = pipe.holders.release(end);
            let unread = (!pipe.holders.held(End::Read) && pipe.held.frames() > 0).then(|| {
                let job = Arc::clone(pipe.held.job());
                core::mem::replace(&mut pipe.held, FrameBytes::new(&job))
            });
            (released, unread)
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
