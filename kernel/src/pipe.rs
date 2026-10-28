use core::ops::Range;

use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::{pmm, DirectMap};

use kernel::message::Window;
use kernel::pipe::{Bytes, End, Holders, Landing, Released, MOST_FRAMES};
use kernel::sched::job::Job;

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


/// What a pipe holds: its bytes, in frames counted to the job that made the
/// pipe, and the heap their list takes, charged to it. The frames go back
/// when it drops.
struct Held {
    bytes: Bytes<u64>,
    list: Charge,
}

impl Held {
    fn new(job: &Arc<Job>) -> Self {
        Self { bytes: Bytes::new(), list: Charge::none(job) }
    }

    /// Take the frames a write of `n` bytes wants, all of them or none.
    fn take_frames(&mut self, n: usize) -> Result<(), Exhausted> {
        let wants = self.bytes.wants(n);
        if wants == 0 {
            return Ok(());
        }
        let frames = self.bytes.frames() + wants;
        if frames > self.bytes.listed() {
            let list = frames.next_power_of_two().min(MOST_FRAMES);
            self.list.resize(charge::array::<u64>(list))?;
            self.bytes.list(list);
        }
        let bytes = &mut self.bytes;
        pmm::alloc_held(self.list.job(), wants, |frame| bytes.add(frame)).then_some(()).ok_or(Exhausted)
    }

    /// Give back the spares a read left past those it keeps.
    fn give_back_surplus(&mut self) {
        // SAFETY: a spare holds no byte, and nothing but this list names it.
        unsafe { pmm::free_held(self.list.job(), self.bytes.surplus()) };
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // SAFETY: nothing but this list names its frames.
        unsafe { pmm::free_held(self.list.job(), self.bytes.clear()) };
    }
}

/// One copy gathered from runs that continue each other in both source and
/// destination, so a user page's piece over two physically adjacent frames
/// is one copy, not two.
struct Gather {
    from: *const u8,
    to: *mut u8,
    len: usize,
}

impl Gather {
    const NONE: Self = Self { from: core::ptr::null(), to: core::ptr::null_mut(), len: 0 };

    /// Add `len` bytes from `from` to `to`, copying what was gathered first
    /// unless these continue it.
    ///
    /// # Safety
    /// `from` is readable and `to` writable for `len` bytes, and stay so
    /// until the next [`Self::flush`]; the two never overlap.
    #[inline(always)]
    unsafe fn add(&mut self, from: *const u8, to: *mut u8, len: usize) {
        if self.len > 0 && self.from.wrapping_add(self.len) == from && self.to.wrapping_add(self.len) == to {
            self.len += len;
            return;
        }
        // SAFETY: what was gathered meets this function's contract.
        unsafe { self.flush() };
        *self = Self { from, to, len };
    }

    /// Copy what was gathered.
    ///
    /// # Safety
    /// As [`Self::add`] for every run gathered.
    #[inline(always)]
    unsafe fn flush(&mut self) {
        if self.len > 0 {
            // SAFETY: the runs gathered are contiguous on both sides and
            // each met `add`'s contract.
            unsafe { core::ptr::copy_nonoverlapping(self.from, self.to, self.len) };
        }
        *self = Self::NONE;
    }
}

/// `within` of a frame a pipe holds, which no process maps and only that
/// pipe's lock reaches.
#[inline(always)]
fn frame_bytes(frame: &u64, within: Range<usize>) -> *mut u8 {
    if within.end > kernel::pipe::FRAME {
        past_a_frame(within);
    }
    // SAFETY: inside the frame, which the direct map covers.
    unsafe { DirectMap::from_phys(*frame).as_mut_ptr::<u8>().add(within.start) }
}

#[cold]
#[inline(never)]
fn past_a_frame(within: Range<usize>) -> ! {
    panic!("pipe: bytes {within:?} past a frame")
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
    held: Held,
    holders: Holders,
    /// Set when a write should hand the next reader transient RT priority — covers readers already runnable, which the wake-time boost misses.
    rt_boost_pending: bool,
    /// Bytes ever written and ever read: the stream positions a connection's
    /// batches are pinned at (`kernel::message`), which never wrap.
    written: u64,
    read: u64,
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
    let held = Held::new(charge.job());
    let pipe = PipeRef(Arc::new(Pipe {
        state: Lock::new(State { held, holders: Holders::new(), rt_boost_pending: false, written: 0, read: 0 }),
        read_watch: Arc::new(Watch::new()),
        write_watch: Arc::new(Watch::new()),
        left: [Arc::new(Watch::new()), Arc::new(Watch::new())],
        _charge: charge,
    }));
    Ok((PipeReader::acquire(&pipe), PipeWriter::acquire(&pipe)))
}

/// A read stopped at a batch pinned where the reader is.
#[derive(Debug)]
pub struct AtBatch;

pub enum PipeWrite {
    /// This many bytes, the stream ending at `end`.
    Wrote { n: usize, end: u64 },
    /// Not the room its landing needs now: wait for `Landing::room`.
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
        self.try_read_within(buf, |_, available| Window::Bytes(available))
            .map(|read| read.expect("a read with no window stops nowhere"))
    }

    /// [`Self::try_read`], taking only what `window` allows of the bytes the
    /// pipe holds, given the reader's stream position: [`AtBatch`] where it
    /// says the reader is at a pinned batch.
    pub fn try_read_within(
        &self,
        buf: &mut UserBytesMut,
        window: impl FnOnce(u64, usize) -> Window,
    ) -> Option<Result<usize, AtBatch>> {
        let (result, boost) = {
            let mut pipe = self.0.state.lock();
            let available = pipe.held.bytes.len();
            let limit = match window(pipe.read, available) {
                Window::AtBatch => return Some(Err(AtBatch)),
                Window::Bytes(limit) => limit,
            };
            if available > 0 && limit > 0 {
                let len = buf.len().min(limit);
                let n = len.min(available);
                let bytes = &mut pipe.held.bytes;
                // One walk of the user window, each pinned piece copied out of
                // the frames it covers.
                buf.write_pieces(0, n, |to, at, piece| {
                    let mut gather = Gather::NONE;
                    bytes.peek(at, piece, |frame, within, off| {
                        // SAFETY: `to` is valid for `piece` bytes until this
                        // walk's callback returns, and `off + within.len()` is
                        // at most `piece`; the frame is this pipe's alone,
                        // reached only under its lock, and no process maps it.
                        unsafe { gather.add(frame_bytes(frame, within.clone()), to.add(off), within.len()) }
                    });
                    // SAFETY: as above, before the callback returns.
                    unsafe { gather.flush() };
                });
                bytes.consume(n);
                pipe.held.give_back_surplus();
                pipe.read += n as u64;
                let boost = core::mem::take(&mut pipe.rt_boost_pending);
                (Some(Ok(n)), boost)
            } else if !pipe.holders.held(End::Write) && available == 0 {
                (Some(Ok(0)), false)
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

    /// The stream position the reader is at: every byte read so far.
    pub fn read_position(&self) -> u64 {
        self.0.state.lock().read
    }

    /// Write `buf` as `landing` has it land, under the one lock hold.
    pub fn write(&self, buf: &UserBytes, landing: Landing) -> PipeWrite {
        let mut pipe = self.0.state.lock();
        if !pipe.holders.held(End::Read) {
            return PipeWrite::BrokenPipe;
        }
        let Some(n) = landing.takes(pipe.held.bytes.space(), buf.len()) else { return PipeWrite::NoRoom };
        if pipe.held.take_frames(n).is_err() {
            return PipeWrite::NoMemory;
        }
        let bytes = &mut pipe.held.bytes;
        // One walk of the user window, each pinned piece copied into the
        // frames it covers.
        buf.read_pieces(0, n, |from, at, piece| {
            let mut gather = Gather::NONE;
            bytes.place(at, piece, |frame, within, off| {
                // SAFETY: `from` is valid for `piece` bytes until this walk's
                // callback returns, and `off + within.len()` is at most `piece`;
                // the frame is this pipe's alone, as above.
                unsafe { gather.add(from.add(off), frame_bytes(frame, within.clone()), within.len()) }
            });
            // SAFETY: as above, before the callback returns.
            unsafe { gather.flush() };
        });
        bytes.commit(n);
        pipe.written += n as u64;
        PipeWrite::Wrote { n, end: pipe.written }
    }

    pub fn has_data(&self) -> bool {
        let pipe = self.0.state.lock();
        pipe.holders.readable(pipe.held.bytes.len())
    }

    /// Whether a write waiting for `want` bytes of room answers now.
    pub fn has_space(&self, want: usize) -> bool {
        let pipe = self.0.state.lock();
        pipe.holders.writable(pipe.held.bytes.space(), want)
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
            let unread = (!pipe.holders.held(End::Read) && pipe.held.bytes.frames() > 0).then(|| {
                let job = Arc::clone(pipe.held.list.job());
                core::mem::replace(&mut pipe.held, Held::new(&job))
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
