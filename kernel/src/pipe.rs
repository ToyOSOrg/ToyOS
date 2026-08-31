use crate::mm::pmm;

use toyos_abi::ring::Ring;

use alloc::sync::Arc;
use alloc::vec::Vec;


use crate::mm::PAGE_2M;
use crate::completion::Watch;
use crate::inbox::InboxId;
use crate::id_map::{IdKey, IdMap};
use crate::sync::Lock;
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::DirectMap;

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
    use super::{close_read, close_write, with_pipes_mut, Pipe, PipeId};

    /// One counted reference to a pipe's read end — `Arc`, for a reader slot.
    pub struct PipeReader(PipeId);

    /// One counted reference to a pipe's write end.
    pub struct PipeWriter(PipeId);

    impl PipeReader {
        /// The only constructor: taking and counting the reference is one statement.
        pub(super) fn acquire(pipe: &mut Pipe) -> Self {
            pipe.readers = pipe.readers.checked_add(1).expect("pipe reader overflow");
            pipe.publish_ends();
            Self(pipe.id)
        }

        pub fn id(&self) -> PipeId { self.0 }
    }

    impl PipeWriter {
        pub(super) fn acquire(pipe: &mut Pipe) -> Self {
            pipe.writers = pipe.writers.checked_add(1).expect("pipe writer overflow");
            pipe.publish_ends();
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
            close_read(self.0);
        }
    }

    impl Drop for PipeWriter {
        fn drop(&mut self) {
            close_write(self.0);
        }
    }
}


pub const PIPE_SIZE: usize = PAGE_2M as usize;

/// A pipe's ring page and the cursors over it — cursors stay in kernel memory
/// because `SYS_PIPE_MAP` maps `page` writable, so anything read back from the page is user-chosen.
struct Backing {
    page: pmm::PhysPage,
    ring: Ring,
}

struct Pipe {
    /// Its own key in `PIPES`, so a handle cannot be built naming a different pipe than the one whose count it bumped.
    id: PipeId,
    /// `None` until first use — allocating eagerly would charge every pending `SYS_CONNECT` 4 MiB before either end sent a byte.
    backing: Option<Backing>,
    readers: u32,
    writers: u32,
    inbox_watchers: Vec<InboxId>,
    /// Held by `Arc` so a blocking site can clone it out from under the table lock and hold it across its own park.
    readers_watch: Arc<Watch>,
    writers_watch: Arc<Watch>,
    /// Set when a write should hand the next reader transient RT priority — covers readers already runnable, which the wake-time boost misses.
    rt_boost_pending: bool,
}

// SAFETY: `Backing::ring`'s raw pointer and its owning `PhysPage` move together in the same struct, and the `PIPES` lock is the only way to reach a `Pipe`, serializing all access to it.
unsafe impl Send for Pipe {}

impl Pipe {
    fn new(id: PipeId) -> Self {
        Self {
            id,
            backing: None,
            readers: 0,
            writers: 0,
            inbox_watchers: Vec::new(),
            readers_watch: Arc::new(Watch::new()),
            writers_watch: Arc::new(Watch::new()),
            rt_boost_pending: false,
        }
    }

    /// Allocate the ring page if this is the first use; `None` on exhaustion, an error return rather than a panic since userland drives it.
    fn back(&mut self) -> Option<&mut Backing> {
        if self.backing.is_none() {
            let page = pmm::alloc_page(pmm::Category::Pipe)?;
            // SAFETY: a fresh 2 MiB page this `Pipe` owns for as long as the `Ring` addresses it.
            let ring = unsafe { Ring::new(page.direct_map().as_mut_ptr(), PIPE_SIZE) };
            self.backing = Some(Backing { page, ring });
            self.publish_ends();
        }
        self.backing.as_mut()
    }

    /// Republish "is the other end gone?" into the mapped header, for netd; the kernel itself decides from its own counts.
    fn publish_ends(&mut self) {
        let Some(backing) = self.backing.as_mut() else { return };
        if self.readers == 0 { backing.ring.close_reader() } else { backing.ring.open_reader() }
        if self.writers == 0 { backing.ring.close_writer() } else { backing.ring.open_writer() }
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
/// Infallible: a pipe with no traffic owns no physical memory, so nothing here can be exhausted; the ring page is allocated on first `try_write` or `map_page`.
pub fn create() -> (PipeReader, PipeWriter) {
    with_pipes_mut(|pipes| {
        let id = pipes.insert_with(Pipe::new);
        let pipe = pipes.get_mut(id).expect("the pipe just inserted");
        let reader = PipeReader::acquire(pipe);
        let writer = PipeWriter::acquire(pipe);
        (reader, writer)
    })
}

/// The pipe's ring page, allocating it if this is its first use.
///
/// `None` when the id names no pipe or its page cannot be allocated — the caller holds a descriptor, so this is physical memory exhaustion.
pub fn map_page(pipe_id: PipeId) -> Option<DirectMap> {
    with_pipes_mut(|pipes| Some(pipes.get_mut(pipe_id)?.back()?.page.direct_map()))
}

pub fn try_read(pipe_id: PipeId, buf: &mut UserBytesMut) -> Option<usize> {
    let (result, boost) = with_pipes_mut(|pipes| {
        let Some(pipe) = pipes.get_mut(pipe_id) else { return (None, false) };
        if pipe.available() > 0 {
            let len = buf.len();
            let n = pipe
                .backing
                .as_mut()
                .expect("available() > 0 implies a ring")
                .ring
                .read(len, |off, src| buf.write_at(off, src));
            let boost = pipe.rt_boost_pending;
            pipe.rt_boost_pending = false;
            (Some(n), boost)
        } else if pipe.writers == 0 {
            (Some(0), false)
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

pub enum PipeWrite {
    Wrote(usize),
    BrokenPipe,
    /// The ring page could not be allocated; distinct from `None`, no wait makes space appear so a caller must not park on it.
    NoMemory,
}

pub fn try_write(pipe_id: PipeId, buf: &UserBytes) -> Option<PipeWrite> {
    with_pipes_mut(|pipes| {
        let pipe = pipes.get_mut(pipe_id)?;
        if pipe.readers == 0 {
            return Some(PipeWrite::BrokenPipe);
        }
        let Some(backing) = pipe.back() else {
            return Some(PipeWrite::NoMemory);
        };
        if backing.ring.space() > 0 {
            Some(PipeWrite::Wrote(backing.ring.write(buf.len(), |off, dst| buf.read_at(off, dst))))
        } else {
            None
        }
    })
}

pub fn has_data(pipe_id: PipeId) -> bool {
    with_pipes(|pipes| {
        pipes.get(pipe_id).is_some_and(|p| p.available() > 0 || p.writers == 0)
    })
}

pub fn has_space(pipe_id: PipeId) -> bool {
    with_pipes(|pipes| {
        pipes.get(pipe_id).is_some_and(|p| p.space() > 0 || p.readers == 0)
    })
}

/// Mark the pipe so the next consumer inherits RT priority.
pub fn set_rt_boost_pending(pipe_id: PipeId) {
    with_pipes_mut(|pipes| {
        if let Some(pipe) = pipes.get_mut(pipe_id) {
            pipe.rt_boost_pending = true;
        }
    });
}

fn close_read(pipe_id: PipeId) {
    // `true` when the pipe still lives and its write end is now the one to wake.
    let wake_writers = with_pipes_mut(|pipes| {
        let pipe = pipes.get_mut(pipe_id).expect("close_read: pipe not found");
        pipe.readers = pipe.readers.checked_sub(1).expect("pipe reader underflow");
        pipe.publish_ends();
        if pipe.readers == 0 && pipe.writers == 0 {
            let pipe = pipes.remove(pipe_id).unwrap();
            free_pipe(pipe);
            false // pipe freed, no one to wake
        } else {
            pipe.readers == 0
        }
    });
    if wake_writers {
        crate::inbox::Source::PipeWritable(pipe_id).wake();
    }
}

fn close_write(pipe_id: PipeId) {
    // `true` when the pipe still lives and its read end is now the one to wake.
    let wake_readers = with_pipes_mut(|pipes| {
        let pipe = pipes.get_mut(pipe_id).expect("close_write: pipe not found");
        pipe.writers = pipe.writers.checked_sub(1).expect("pipe writer underflow");
        pipe.publish_ends();
        if pipe.readers == 0 && pipe.writers == 0 {
            let pipe = pipes.remove(pipe_id).unwrap();
            free_pipe(pipe);
            false // pipe freed, no one to wake
        } else {
            pipe.writers == 0
        }
    });
    if wake_readers {
        crate::inbox::Source::PipeReadable(pipe_id).wake();
    }
}

fn free_pipe(pipe: Pipe) {
    drop(pipe); // PhysPage freed via Drop
}

pub fn add_inbox_watcher(pipe_id: PipeId, inbox_id: InboxId) {
    with_pipes_mut(|pipes| {
        if let Some(pipe) = pipes.get_mut(pipe_id) {
            if !pipe.inbox_watchers.contains(&inbox_id) {
                pipe.inbox_watchers.push(inbox_id);
            }
        }
    });
}

pub fn remove_inbox_watcher(pipe_id: PipeId, inbox_id: InboxId) {
    with_pipes_mut(|pipes| {
        if let Some(pipe) = pipes.get_mut(pipe_id) {
            pipe.inbox_watchers.retain(|&id| id != inbox_id);
        }
    });
}

/// The waiter set of this pipe's read end, cloned out for a blocking or wake path to hold on its own stack.
pub fn readers_queue(pipe_id: PipeId) -> Option<PipeEnd> {
    with_pipes(|pipes| {
        pipes.get(pipe_id).map(|p| PipeEnd {
            watch: p.readers_watch.clone(),
        })
    })
}

pub fn writers_queue(pipe_id: PipeId) -> Option<PipeEnd> {
    with_pipes(|pipes| {
        pipes.get(pipe_id).map(|p| PipeEnd {
            watch: p.writers_watch.clone(),
        })
    })
}

/// One end of a pipe: the queue a blocking site registers on and the subject it arms.
pub struct PipeEnd {
    pub watch: Arc<Watch>,
}

pub fn inbox_watchers(pipe_id: PipeId) -> Vec<InboxId> {
    with_pipes(|pipes| {
        pipes.get(pipe_id).map_or(Vec::new(), |p| p.inbox_watchers.clone())
    })
}
