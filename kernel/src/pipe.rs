use crate::mm::pmm;

use kernel::message::Window;
use kernel::pipe::{End, Holders, Landing, Released};
use toyos_abi::ring::Ring;

use alloc::sync::Arc;


use crate::mm::PAGE_2M;
use crate::watch::Watch;
use crate::id_map::{IdKey, IdMap};
use crate::sync::Lock;
use crate::user_ptr::{UserBytes, UserBytesMut};

/// Raw pipe identifier; carries no refcount, not exposed outside the kernel.

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PipeId(usize);

impl PipeId {
}

impl core::ops::Add for PipeId {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { PipeId(self.0 + rhs.0) }
}

impl IdKey for PipeId {
    const ZERO: Self = PipeId(0);
    const ONE: Self = PipeId(1);
}

pub use handle::{PipeReader, PipeWriter};

/// Owned references to a pipe's two ends.
///
/// The id inside each handle is private to this module; only `acquire`, which
/// requires the `&mut Pipe` a `PIPES` lock produces, may look up a pipe and
/// take a counted reference, so no program point exists between the two where
/// the last other end could close and free it.
mod handle {
    use super::{release, with_pipes_mut, End, Pipe, PipeId};

    /// One counted reference to a pipe's read end — `Arc`, for a reader slot.
    pub struct PipeReader(PipeId);

    /// One counted reference to a pipe's write end.
    pub struct PipeWriter(PipeId);

    impl PipeReader {
        /// The only constructor: taking and counting the reference is one statement.
        pub(super) fn acquire(pipe: &mut Pipe) -> Self {
            pipe.hold(End::Read);
            Self(pipe.id)
        }

        pub fn id(&self) -> PipeId { self.0 }
    }

    impl PipeWriter {
        pub(super) fn acquire(pipe: &mut Pipe) -> Self {
            pipe.hold(End::Write);
            Self(pipe.id)
        }

        pub fn id(&self) -> PipeId { self.0 }
    }

    /// A live handle proves the pipe is still in the table, so `expect` here cannot fail on a race.
    impl Clone for PipeReader {
        fn clone(&self) -> Self {
            with_pipes_mut(|pipes| {
                Self::acquire(pipes.get_mut(self.0).expect("a held PipeReader's pipe is in the table"))
            })
        }
    }

    impl Clone for PipeWriter {
        fn clone(&self) -> Self {
            with_pipes_mut(|pipes| {
                Self::acquire(pipes.get_mut(self.0).expect("a held PipeWriter's pipe is in the table"))
            })
        }
    }

    impl Drop for PipeReader {
        fn drop(&mut self) {
            release(self.0, End::Read);
        }
    }

    impl Drop for PipeWriter {
        fn drop(&mut self) {
            release(self.0, End::Write);
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
    /// Its own key in `PIPES`, so a handle cannot be built naming a different pipe than the one whose count it bumped.
    id: PipeId,
    /// `None` until first use — allocating eagerly would charge every pending `SYS_CONNECT` 4 MiB before either end sent a byte.
    backing: Option<Backing>,
    holders: Holders,
    /// The read end's and the write end's watches. Held by `Arc` so a blocking site or a
    /// poll registration can clone one out from under the table lock and hold it across its own park.
    read_watch: Arc<Watch>,
    write_watch: Arc<Watch>,
    /// Each end's leaving watch, by [`End`]: an `OTHER_END_GONE` poll on that
    /// end sits on it, and only the other end's last holder posts it — a byte
    /// moved through the pipe wakes no poll that asked about a leaving.
    left: [Arc<Watch>; 2],
    /// Set when a write should hand the next reader transient RT priority — covers readers already runnable, which the wake-time boost misses.
    rt_boost_pending: bool,
    /// Bytes ever written and ever read: the stream positions a connection's
    /// batches are pinned at (`kernel::message`), which never wrap.
    written: u64,
    read: u64,
}

// SAFETY: `Backing::ring`'s raw pointer and its owning `PhysPage` move together in the same struct, and the `PIPES` lock is the only way to reach a `Pipe`, serializing all access to it.
unsafe impl Send for Pipe {}

impl Pipe {
    fn new(id: PipeId) -> Self {
        Self {
            id,
            backing: None,
            holders: Holders::new(),
            read_watch: Arc::new(Watch::new()),
            write_watch: Arc::new(Watch::new()),
            left: [Arc::new(Watch::new()), Arc::new(Watch::new())],
            rt_boost_pending: false,
            written: 0,
            read: 0,
        }
    }

    /// Allocate the ring page if this is the first use; `None` on exhaustion, an error return rather than a panic since userland drives it.
    fn back(&mut self) -> Option<&mut Backing> {
        if self.backing.is_none() {
            let page = pmm::alloc_page()?;
            // SAFETY: a fresh 2 MiB page this `Pipe` owns for as long as the `Ring` addresses it.
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

    fn hold(&mut self, end: End) {
        self.holders.hold(end);
        self.publish_ends();
    }

    fn available(&self) -> u32 {
        self.backing.as_ref().map_or(0, |b| b.ring.available())
    }

    /// A pipe with no page yet reports its whole capacity free — the allocation is deferred, not refused.
    fn space(&self) -> u32 {
        self.backing.as_ref().map_or(u32::MAX, |b| b.ring.space())
    }
}

static PIPES: Lock<Option<IdMap<PipeId, Pipe>>> = Lock::new(None);

fn with_pipes<R>(f: impl FnOnce(&IdMap<PipeId, Pipe>) -> R) -> R {
    let guard = PIPES.lock();
    f(guard.as_ref().expect("pipes not initialized"))
}

fn with_pipes_mut<R>(f: impl FnOnce(&mut IdMap<PipeId, Pipe>) -> R) -> R {
    let mut guard = PIPES.lock();
    f(guard.as_mut().expect("pipes not initialized"))
}

pub fn init() {
    *PIPES.lock() = Some(IdMap::new());
}

/// Create a new pipe. Returns owned reader + writer references.
///
/// Infallible: a pipe with no traffic owns no physical memory, so nothing here can be exhausted; the ring page is allocated on first `write` or `map_page`.
pub fn create() -> (PipeReader, PipeWriter) {
    with_pipes_mut(|pipes| {
        let id = pipes.insert_with(Pipe::new);
        let pipe = pipes.get_mut(id).expect("the pipe just inserted");
        let reader = PipeReader::acquire(pipe);
        let writer = PipeWriter::acquire(pipe);
        (reader, writer)
    })
}

pub fn try_read(pipe_id: PipeId, buf: &mut UserBytesMut) -> Option<usize> {
    try_read_within(pipe_id, buf, |_, available| Window::Bytes(available))
        .map(|read| read.expect("a read with no window stops nowhere"))
}

/// A read stopped at a batch pinned where the reader is.
#[derive(Debug)]
pub struct AtBatch;

/// [`try_read`], taking only what `window` allows of the bytes the ring
/// holds, given the reader's stream position: [`AtBatch`] where it says the
/// reader is at a pinned batch.
pub fn try_read_within(
    pipe_id: PipeId,
    buf: &mut UserBytesMut,
    window: impl FnOnce(u64, usize) -> Window,
) -> Option<Result<usize, AtBatch>> {
    let (result, boost) = with_pipes_mut(|pipes| {
        let Some(pipe) = pipes.get_mut(pipe_id) else { return (None, false) };
        let limit = match window(pipe.read, pipe.available() as usize) {
            Window::AtBatch => return (Some(Err(AtBatch)), false),
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
            let boost = pipe.rt_boost_pending;
            pipe.rt_boost_pending = false;
            (Some(Ok(n)), boost)
        } else if !pipe.holders.held(End::Write) && pipe.available() == 0 {
            (Some(Ok(0)), false)
        } else {
            (None, false)
        }
    });
    if boost {
        // Outside the PIPES lock: the scheduler takes its own CPU-queue lock.
        crate::scheduler::boost_current_rt_inherited();
    }
    result
}

/// The stream position the reader is at: every byte read so far.
pub fn read_position(pipe_id: PipeId) -> Option<u64> {
    with_pipes(|pipes| pipes.get(pipe_id).map(|p| p.read))
}

pub enum PipeWrite {
    /// This many bytes, the stream ending at `end`.
    Wrote { n: usize, end: u64 },
    /// Not the room its landing needs now: wait for `Landing::room`.
    NoRoom,
    BrokenPipe,
    /// The ring page could not be allocated; no wait makes space appear, so a caller must not park on it.
    NoMemory,
}

/// Write `buf` as `landing` has it land, under the one lock hold.
pub fn write(pipe_id: PipeId, buf: &UserBytes, landing: Landing) -> PipeWrite {
    with_pipes_mut(|pipes| {
        let Some(pipe) = pipes.get_mut(pipe_id) else { return PipeWrite::BrokenPipe };
        if !pipe.holders.held(End::Read) {
            return PipeWrite::BrokenPipe;
        }
        let Some(backing) = pipe.back() else { return PipeWrite::NoMemory };
        let Some(n) = landing.takes(backing.ring.space(), buf.len()) else { return PipeWrite::NoRoom };
        let wrote = backing.ring.write(n, |off, mut dst| buf.read_run(off, &mut dst));
        assert_eq!(wrote, n, "pipe: a ring with room for a write took less of it");
        pipe.written += n as u64;
        PipeWrite::Wrote { n, end: pipe.written }
    })
}

pub fn has_data(pipe_id: PipeId) -> bool {
    with_pipes(|pipes| {
        pipes.get(pipe_id).is_some_and(|p| p.holders.readable(p.available()))
    })
}

/// Whether a write waiting for `want` bytes of room answers now.
pub fn has_space(pipe_id: PipeId, want: usize) -> bool {
    with_pipes(|pipes| pipes.get(pipe_id).is_some_and(|p| p.holders.writable(p.space(), want)))
}

/// Whether the end opposite `end` of this pipe has no holder left.
pub fn other_end_gone(pipe_id: PipeId, end: End) -> bool {
    with_pipes(|pipes| pipes.get(pipe_id).is_some_and(|p| p.holders.other_end_gone(end)))
}

/// Mark the pipe so the next consumer inherits RT priority.
pub fn set_rt_boost_pending(pipe_id: PipeId) {
    with_pipes_mut(|pipes| {
        if let Some(pipe) = pipes.get_mut(pipe_id) {
            pipe.rt_boost_pending = true;
        }
    });
}

/// One reference to `end` lets go; its end's last posts the other end's watch
/// and its leaving watch.
fn release(pipe_id: PipeId, end: End) {
    let (released, left) = with_pipes_mut(|pipes| {
        let pipe = pipes.get_mut(pipe_id).expect("release: pipe not found");
        let released = pipe.holders.release(end);
        pipe.publish_ends();
        let left = pipe.left[slot(end.other())].clone();
        if released == Released::PipeGone {
            let pipe = pipes.remove(pipe_id).unwrap();
            free_pipe(pipe);
        }
        (released, left)
    });
    match (released, end) {
        (Released::Held | Released::PipeGone, _) => return,
        (Released::EndGone, End::Read) => crate::scheduler::wake_pipe_writers(pipe_id),
        (Released::EndGone, End::Write) => crate::scheduler::wake_pipe_readers(pipe_id),
    }
    left.post();
}

fn slot(end: End) -> usize {
    match end {
        End::Read => 0,
        End::Write => 1,
    }
}

fn free_pipe(pipe: Pipe) {
    drop(pipe); // PhysPage freed via Drop
}

/// The read end's watch, cloned out for a blocking site, a post or a poll to hold on its own stack.
pub fn read_watch(pipe_id: PipeId) -> Option<Arc<Watch>> {
    with_pipes(|pipes| pipes.get(pipe_id).map(|p| p.read_watch.clone()))
}

/// The write end's watch, the same way.
pub fn write_watch(pipe_id: PipeId) -> Option<Arc<Watch>> {
    with_pipes(|pipes| pipes.get(pipe_id).map(|p| p.write_watch.clone()))
}

/// `end`'s leaving watch, the same way: posted once, by the other end's last
/// holder.
pub fn left_watch(pipe_id: PipeId, end: End) -> Option<Arc<Watch>> {
    with_pipes(|pipes| pipes.get(pipe_id).map(|p| p.left[slot(end)].clone()))
}
