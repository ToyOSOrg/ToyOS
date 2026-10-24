use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::pmm;

use kernel::message::Window;
use kernel::pipe::{End, Holders, Landing, Released};
use toyos_abi::ring::Ring;

use alloc::sync::Arc;


use crate::mm::PAGE_2M;
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


pub const PIPE_SIZE: usize = PAGE_2M as usize;

const _: () = assert!(
    toyos_abi::syscall::MAX_WHOLE_PIPE_WRITE <= PIPE_SIZE - core::mem::size_of::<toyos_abi::ring::RingHeader>(),
    "a whole pipe write must fit the ring",
);

/// A pipe's ring page and the cursors over it, in kernel memory.
struct Backing {
    /// Owns the page `ring` addresses.
    _page: pmm::PhysPage,
    ring: Ring,
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
    /// Its own heap and its watches.
    _charge: Charge,
}

/// What a pipe's lock holds.
struct State {
    /// `None` until first use — allocating eagerly would charge every pending
    /// `SYS_CONNECT` 4 MiB before either end sent a byte — and again once
    /// neither end is held.
    backing: Option<Backing>,
    holders: Holders,
    /// Set when a write should hand the next reader transient RT priority — covers readers already runnable, which the wake-time boost misses.
    rt_boost_pending: bool,
    /// Bytes ever written and ever read: the stream positions a connection's
    /// batches are pinned at (`kernel::message`), which never wrap.
    written: u64,
    read: u64,
}

// SAFETY: `Backing::ring`'s raw pointer and its owning `PhysPage` move together in the same struct, and the pipe's lock is the only way to reach a `State`, serializing all access to it.
unsafe impl Send for State {}

impl Pipe {
    /// The heap one takes: itself and its four watches.
    fn heap() -> usize {
        charge::arc::<Pipe>() + 4 * charge::arc::<Watch>()
    }
}

impl State {
    /// Allocate the ring page if this is the first use; `None` where the
    /// kernel holds back what is left, which userland's write is refused.
    fn back(&mut self) -> Option<&mut Backing> {
        if self.backing.is_none() {
            let page = pmm::alloc_page_for_process()?;
            // SAFETY: a fresh 2 MiB page this `State` owns for as long as the `Ring` addresses it.
            let ring = unsafe { Ring::new(page.direct_map().as_mut_ptr(), PIPE_SIZE) };
            self.backing = Some(Backing { _page: page, ring });
            self.publish_ends();
        }
        self.backing.as_mut()
    }

    /// Republish "is the other end gone?" into the mapped header, for netstack; the kernel itself decides from its own counts.
    fn publish_ends(&mut self) {
        let Some(backing) = self.backing.as_mut() else { return };
        if self.holders.held(End::Read) { backing.ring.open_reader() } else { backing.ring.close_reader() }
        if self.holders.held(End::Write) { backing.ring.open_writer() } else { backing.ring.close_writer() }
    }

    fn available(&self) -> u32 {
        self.backing.as_ref().map_or(0, |b| b.ring.available())
    }

    /// A pipe with no page yet reports its whole capacity free — the allocation is deferred, not refused.
    fn space(&self) -> u32 {
        self.backing.as_ref().map_or(u32::MAX, |b| b.ring.space())
    }
}

/// Create a new pipe. Returns owned reader + writer references.
///
/// A pipe with no traffic owns no physical memory: its ring page is
/// allocated at its first `write`, which is refused if none is left.
pub fn create() -> Result<(PipeReader, PipeWriter), Exhausted> {
    let charge = Charge::new(Pipe::heap())?;
    let pipe = PipeRef(Arc::new(Pipe {
        state: Lock::new(State { backing: None, holders: Holders::new(), rt_boost_pending: false, written: 0, read: 0 }),
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
    /// The ring page could not be allocated; no wait makes space appear, so a caller must not park on it.
    NoMemory,
}

impl PipeRef {
    fn hold(&self, end: End) {
        let mut state = self.0.state.lock();
        state.holders.hold(end);
        state.publish_ends();
    }

    pub fn try_read(&self, buf: &mut UserBytesMut) -> Option<usize> {
        self.try_read_within(buf, |_, available| Window::Bytes(available))
            .map(|read| read.expect("a read with no window stops nowhere"))
    }

    /// [`Self::try_read`], taking only what `window` allows of the bytes the
    /// ring holds, given the reader's stream position: [`AtBatch`] where it
    /// says the reader is at a pinned batch.
    pub fn try_read_within(
        &self,
        buf: &mut UserBytesMut,
        window: impl FnOnce(u64, usize) -> Window,
    ) -> Option<Result<usize, AtBatch>> {
        let (result, boost) = {
            let mut pipe = self.0.state.lock();
            let limit = match window(pipe.read, pipe.available() as usize) {
                Window::AtBatch => return Some(Err(AtBatch)),
                Window::Bytes(limit) => limit,
            };
            if pipe.available() > 0 && limit > 0 {
                let len = buf.len().min(limit);
                let n = pipe
                    .backing
                    .as_mut()
                    .expect("available() > 0 implies a ring")
                    .ring
                    .read(len, |off, src| buf.write_run(off, &src));
                pipe.read += n as u64;
                let boost = core::mem::take(&mut pipe.rt_boost_pending);
                (Some(Ok(n)), boost)
            } else if !pipe.holders.held(End::Write) && pipe.available() == 0 {
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
        let Some(backing) = pipe.back() else { return PipeWrite::NoMemory };
        let Some(n) = landing.takes(backing.ring.space(), buf.len()) else { return PipeWrite::NoRoom };
        let wrote = backing.ring.write(n, |off, mut dst| buf.read_run(off, &mut dst));
        assert_eq!(wrote, n, "pipe: a ring with room for a write took less of it");
        pipe.written += n as u64;
        PipeWrite::Wrote { n, end: pipe.written }
    }

    pub fn has_data(&self) -> bool {
        let pipe = self.0.state.lock();
        pipe.holders.readable(pipe.available())
    }

    /// Whether a write waiting for `want` bytes of room answers now.
    pub fn has_space(&self, want: usize) -> bool {
        let pipe = self.0.state.lock();
        pipe.holders.writable(pipe.space(), want)
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
    /// watch and its leaving watch, and the last of either end frees the ring.
    fn release(&self, end: End) {
        let (released, page) = {
            let mut pipe = self.0.state.lock();
            let released = pipe.holders.release(end);
            pipe.publish_ends();
            // Dropped outside the lock: the page goes back to the allocator.
            let page = (released == Released::PipeGone).then(|| pipe.backing.take()).flatten();
            (released, page)
        };
        drop(page);
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
