//! The cache of a volume's own structures — a btree node, a FAT sector — as
//! a [`BlockDev`] over the disk. A file's data is the paged objects' and
//! passes it by ([`BlockDev::read_data`], [`BlockDev::write_data`]): read
//! from a block the cache holds, written into one it holds, and otherwise
//! straight to and from the disk, never kept.
//!
//! **A write lands here and reaches the disk at a flush.** A flush writes
//! every dirty block in runs, as many at once as the disk takes
//! ([`Disk::depth`]), handing the disk the cache's own blocks to share
//! ([`Disk::write_shared`]), then asks the disk to make them durable; a
//! volume's sync is its own metadata written into the cache and then this. A
//! write that would take the cache past [`DIRTY_LIMIT`] dirty blocks flushes
//! first, and is refused whole when that flush is, so memory bounds the
//! write-back and not the other way round.
//!
//! **Clean blocks are kept up to the limit the cache is made with** and the
//! oldest-read goes first; a dirty block is never dropped. A block read in
//! is kept only if the allocator gives it and its place in the eviction order
//! room: refused, the read is answered from the disk's bytes and nothing is
//! cached. A block a flush cleans whose place is refused is dropped, since the
//! disk holds it. The block map's node, a small allocation every few inserts,
//! and the block's share count are the parts of keeping a block read in that
//! still cannot be refused.
//!
//! **Several tasks may use it at once**, and the disk never has two requests
//! for one block in flight when either is a write, which the block service's
//! client forbids because requests in flight together are unordered. One
//! flush runs at a time; a second waits for it and then takes its own dirty
//! set. A flush writes no block that a fetch is reading. A fetch reads only
//! blocks the cache does not hold, so a flush's blocks, which stay held while
//! it writes them, are never fetched under it. A block written while a flush
//! carries an older copy of it stays dirty. No borrow of the cache's state is
//! held across a disk request.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Poll, Waker};

use toyos_blockdev::{within, Block, BlockDev, Error, SharedBlock, BLOCK};

/// A disk this server owns, and what to do when nothing can go on until it
/// answers.
// Every future runs on the server's one thread, as `BlockDev`'s do.
#[allow(async_fn_in_trait)]
pub trait Disk: BlockDev {
    /// Returns once a request asked of this disk may have been answered. A
    /// disk that answers every request as it is asked is never waited on.
    fn wait(&self) {
        panic!("fileserver: a disk that answers as it is asked was waited on");
    }

    /// How many writes a flush keeps in flight at once: at least one.
    fn depth(&self) -> usize {
        1
    }

    /// `blocks` written at `lba`: shared by a disk that keeps a write's bytes
    /// until a flush covers it, copied by one that does not.
    async fn write_shared(&self, lba: u64, blocks: &[SharedBlock]) -> Result<(), Error> {
        let data: Vec<Block> = blocks.iter().map(|block| ***block).collect();
        self.write(lba, &data).await
    }
}

impl Disk for diskserver::disk::Served {
    fn wait(&self) {
        diskserver::disk::Served::wait(self)
    }

    fn depth(&self) -> usize {
        diskserver::disk::Served::depth(self)
    }

    async fn write_shared(&self, lba: u64, blocks: &[SharedBlock]) -> Result<(), Error> {
        diskserver::disk::Served::write_shared(self, lba, blocks).await
    }
}

/// Dirty blocks held before the cache flushes by itself: 8 MiB.
pub const DIRTY_LIMIT: usize = 2 * 1024;

// A cache that flushes by its own dirty bound never meets a session's.
const _: () = assert!(DIRTY_LIMIT as u64 <= diskserver::session::KEPT_BLOCKS);

/// The longest run one disk request carries, in blocks.
const RUN: usize = 32;

struct Slot {
    /// Shared with the disk while it keeps a write of it: a write here then
    /// takes a block of its own.
    data: SharedBlock,
    dirty: bool,
    /// Which write put `data` here, so a flush that carried an older copy
    /// leaves the block dirty.
    version: u64,
}

/// Blocks being read from the disk into the cache.
struct Fetch {
    first: u64,
    count: usize,
}

#[derive(Default)]
struct Inner {
    /// The most clean blocks kept.
    clean_limit: usize,
    slots: BTreeMap<u64, Slot>,
    /// Clean blocks in the order they were read or cleaned; stale entries are
    /// skipped when popped.
    order: VecDeque<u64>,
    dirty: usize,
    versions: u64,
    fetches: BTreeMap<u64, Fetch>,
    fetched: u64,
    flushing: bool,
    /// Writes the disk completed, counted from 1 at mount, since a previous
    /// server may have written it; and how many of them the last disk flush
    /// that answered was asked after. A data write lands outside any flush.
    written: u64,
    flushed: u64,
    /// Tasks waiting for a flush to end or a fetch to land.
    waiting: Vec<Waker>,
    reads: u64,
    hits: u64,
}

pub struct Cache<D> {
    disk: D,
    inner: RefCell<Inner>,
}

/// What a cache has done, for the server's `inspect` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub reads: u64,
    pub hits: u64,
    pub cached: usize,
    pub dirty: usize,
}

impl<D: Disk> Cache<D> {
    /// A cache over `disk` keeping at most `clean_limit` clean blocks.
    pub fn new(disk: D, clean_limit: usize) -> Self {
        Self { disk, inner: RefCell::new(Inner { clean_limit, written: 1, ..Inner::default() }) }
    }

    /// The disk, once nothing else holds the cache: what it holds is what the
    /// last flush left.
    pub fn into_disk(self) -> D {
        self.disk
    }

    pub fn disk(&self) -> &D {
        &self.disk
    }

    pub fn counts(&self) -> Counts {
        let inner = self.inner.borrow();
        Counts { reads: inner.reads, hits: inner.hits, cached: inner.slots.len(), dirty: inner.dirty }
    }

    /// `future` driven to its end on this thread, waiting on the disk when it
    /// can go no further: where a synchronous caller meets the cache.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        toyos_blockdev::block_on(future, || self.disk.wait())
    }

    /// Blocks `first..first + out.len()` from the cache, a block that is not
    /// there fetched into it first, in runs.
    async fn cached(&self, first: u64, out: &mut [Block]) -> Result<(), Error> {
        within(first, out.len(), self.disk.blocks())?;
        let count = out.len();
        let mut i = 0;
        while i < count {
            let block = first + i as u64;
            let (run, id) = {
                let mut inner = self.inner.borrow_mut();
                inner.reads += 1;
                if let Some(slot) = inner.slots.get(&block) {
                    out[i] = **slot.data;
                    inner.hits += 1;
                    i += 1;
                    continue;
                }
                // The run of misses from here, which one request fetches.
                let mut run = 1;
                while i + run < count && run < RUN && !inner.slots.contains_key(&(block + run as u64)) {
                    run += 1;
                }
                let id = inner.fetched;
                inner.fetched += 1;
                inner.fetches.insert(id, Fetch { first: block, count: run });
                (run, id)
            };
            let span = &mut out[i..i + run];
            let read = self.disk.read(block, span).await;
            self.inner.borrow_mut().fetches.remove(&id).expect("fileserver: a fetch outlived its record");
            self.wake();
            read?;
            let mut inner = self.inner.borrow_mut();
            for (k, data) in span.iter_mut().enumerate() {
                let at = block + k as u64;
                // A block written while this was asked is newer than what the
                // disk answered, and stays held until a flush that waited for
                // this fetch has written it.
                match inner.slots.get(&at) {
                    Some(slot) => *data = **slot.data,
                    None => {
                        if inner.order.try_reserve(1).is_ok() {
                            if let Some(data) = boxed(data) {
                                inner.slots.insert(at, Slot { data: Rc::new(data), dirty: false, version: 0 });
                                inner.order.push_back(at);
                            }
                        }
                    }
                }
            }
            i += run;
        }
        self.inner.borrow_mut().evict();
        Ok(())
    }

    /// Pending until no fetch reads a block of `first..first + count`: a data
    /// request never meets one of the cache's own in flight.
    async fn no_fetch_of(&self, first: u64, count: usize) {
        let end = first + count as u64;
        self.until(|inner| !inner.fetches.values().any(|f| f.first < end && first < f.first + f.count as u64)).await
    }

    /// Pending until `ready` holds of the cache's state, rechecked whenever a
    /// flush ends or a fetch lands.
    async fn until(&self, ready: impl Fn(&Inner) -> bool) {
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if ready(&inner) {
                return Poll::Ready(());
            }
            inner.waiting.push(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn wake(&self) {
        let waiting = std::mem::take(&mut self.inner.borrow_mut().waiting);
        waiting.into_iter().for_each(Waker::wake);
    }
}

/// `data` in a block of its own, or `None` where the allocator has no room.
fn boxed(data: &Block) -> Option<Box<Block>> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(BLOCK).ok()?;
    bytes.extend_from_slice(data);
    Some(bytes.into_boxed_slice().try_into().expect("a block's bytes"))
}

/// The one flush running, until this is dropped: when it finishes, is refused
/// or is abandoned.
struct Flushing<'a, D: Disk>(&'a Cache<D>);

impl<D: Disk> Drop for Flushing<'_, D> {
    fn drop(&mut self) {
        self.0.inner.borrow_mut().flushing = false;
        self.0.wake();
    }
}

impl<D: Disk> BlockDev for Cache<D> {
    fn blocks(&self) -> u64 {
        self.disk.blocks()
    }

    /// Each block from the cache where it is there and in runs from the disk
    /// where it is not.
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.cached(lba, out).await
    }

    /// From the cache where it holds the block, which is then the newest
    /// copy, and otherwise from the disk in runs, keeping nothing.
    async fn read_data(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.disk.blocks())?;
        self.no_fetch_of(lba, out.len()).await;
        let mut i = 0;
        while i < out.len() {
            let run = {
                let inner = self.inner.borrow();
                if let Some(slot) = inner.slots.get(&(lba + i as u64)) {
                    out[i] = **slot.data;
                    i += 1;
                    continue;
                }
                (1..out.len() - i).take_while(|k| !inner.slots.contains_key(&(lba + (i + k) as u64))).count() + 1
            };
            self.disk.read(lba + i as u64, &mut out[i..i + run]).await?;
            i += run;
        }
        Ok(())
    }

    /// Into a block the cache holds, as [`write`](BlockDev::write) does, and
    /// otherwise to the disk at once: durable at the next flush either way.
    async fn write_data(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        within(lba, data.len(), self.disk.blocks())?;
        self.no_fetch_of(lba, data.len()).await;
        let mut i = 0;
        while i < data.len() {
            let (held, run) = {
                let inner = self.inner.borrow();
                let held = inner.slots.contains_key(&(lba + i as u64));
                let run = (1..data.len() - i).take_while(|k| inner.slots.contains_key(&(lba + (i + k) as u64)) == held).count() + 1;
                (held, run)
            };
            let (at, blocks) = (lba + i as u64, &data[i..i + run]);
            if held {
                self.write(at, blocks).await?;
            } else {
                // A flush finds nothing of these to write, and must still ask
                // the disk to keep them.
                let wrote = self.disk.write(at, blocks).await;
                self.inner.borrow_mut().written += 1;
                wrote?;
            }
            i += run;
        }
        Ok(())
    }

    /// Into the cache: on the disk at the next flush.
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        within(lba, data.len(), self.disk.blocks())?;
        let over = {
            let inner = self.inner.borrow();
            let newly = (lba..lba + data.len() as u64).filter(|b| !inner.slots.get(b).is_some_and(|s| s.dirty)).count();
            inner.dirty + newly > DIRTY_LIMIT
        };
        if over {
            self.flush().await?;
        }
        let mut inner = self.inner.borrow_mut();
        for (block, chunk) in (lba..).zip(data) {
            inner.versions += 1;
            let version = inner.versions;
            let newly = match inner.slots.get_mut(&block) {
                Some(slot) => {
                    match Rc::get_mut(&mut slot.data) {
                        Some(data) => **data = *chunk,
                        None => slot.data = Rc::new(Box::new(*chunk)),
                    }
                    slot.version = version;
                    !std::mem::replace(&mut slot.dirty, true)
                }
                None => {
                    inner.slots.insert(block, Slot { data: Rc::new(Box::new(*chunk)), dirty: true, version });
                    true
                }
            };
            if newly {
                inner.dirty += 1;
            }
        }
        Ok(())
    }

    /// Every dirty block to the disk in runs, up to [`Disk::depth`] of them in
    /// flight, then the disk made durable. A block the disk refused stays
    /// dirty, and once one run is refused no other is begun. A disk written
    /// nothing since the last flush it answered is asked nothing: every write
    /// it took is already durable.
    async fn flush(&self) -> Result<(), Error> {
        self.until(|inner| !inner.flushing).await;
        self.inner.borrow_mut().flushing = true;
        let _flushing = Flushing(self);
        let mut runs = self.dirty_runs().into_iter();
        let mut flight: Vec<Pin<Box<dyn Future<Output = Result<(), Error>> + '_>>> = Vec::new();
        let mut refused = Ok(());
        poll_fn(|cx| loop {
            while refused.is_ok() && flight.len() < self.disk.depth() {
                let Some((first, count)) = runs.next() else { break };
                flight.push(Box::pin(self.write_run(first, count)));
            }
            let before = flight.len();
            flight.retain_mut(|run| match run.as_mut().poll(cx) {
                Poll::Ready(done) => {
                    refused = std::mem::replace(&mut refused, Ok(())).and(done);
                    false
                }
                Poll::Pending => true,
            });
            if flight.is_empty() && (refused.is_err() || runs.len() == 0) {
                return Poll::Ready(());
            }
            if flight.len() == before {
                return Poll::Pending;
            }
        })
        .await;
        refused?;
        let written = self.inner.borrow().written;
        if written > self.inner.borrow().flushed {
            self.disk.flush().await?;
            self.inner.borrow_mut().flushed = written;
        }
        self.inner.borrow_mut().evict();
        Ok(())
    }
}

impl<D: Disk> Cache<D> {
    /// The dirty blocks as runs of consecutive blocks, each at most [`RUN`].
    fn dirty_runs(&self) -> Vec<(u64, usize)> {
        let mut runs: Vec<(u64, usize)> = Vec::new();
        for (&block, _) in self.inner.borrow().slots.iter().filter(|(_, s)| s.dirty) {
            match runs.last_mut() {
                Some((first, count)) if *first + *count as u64 == block && *count < RUN => *count += 1,
                _ => runs.push((block, 1)),
            }
        }
        runs
    }

    /// One run of a flush: written once no fetch is reading it, and each block
    /// clean afterwards unless written again meanwhile.
    async fn write_run(&self, first: u64, count: usize) -> Result<(), Error> {
        let end = first + count as u64;
        self.until(|inner| !inner.fetches.values().any(|f| f.first < end && first < f.first + f.count as u64)).await;
        let (blocks, versions): (Vec<SharedBlock>, Vec<u64>) = {
            let inner = self.inner.borrow();
            (first..end)
                .map(|block| {
                    // Only a flush cleans a block, and this is the one.
                    let slot = inner.slots.get(&block).expect("fileserver: a dirty block left the cache during its flush");
                    (Rc::clone(&slot.data), slot.version)
                })
                .unzip()
        };
        let wrote = self.disk.write_shared(first, &blocks).await;
        self.inner.borrow_mut().written += 1;
        wrote?;
        let mut inner = self.inner.borrow_mut();
        for (block, version) in (first..).zip(versions) {
            // A block written again meanwhile stays dirty.
            let slot = inner.slots.get_mut(&block).expect("fileserver: a dirty block left the cache during its flush");
            if slot.version == version {
                slot.dirty = false;
                inner.dirty -= 1;
                // The disk holds it now, so a block whose place in the
                // eviction order is refused is dropped rather than kept.
                if inner.order.try_reserve(1).is_ok() {
                    inner.order.push_back(block);
                } else {
                    inner.slots.remove(&block);
                }
            }
        }
        Ok(())
    }
}

impl Inner {
    /// Drop the oldest clean blocks past the limit.
    fn evict(&mut self) {
        while self.slots.len() - self.dirty > self.clean_limit {
            let Some(block) = self.order.pop_front() else { break };
            if self.slots.get(&block).is_some_and(|s| !s.dirty) {
                self.slots.remove(&block);
            }
        }
        // A queue of mostly stale entries is compacted, so it stays the size
        // of what it orders.
        if self.order.len() > 4 * (self.clean_limit + DIRTY_LIMIT) {
            let slots = &self.slots;
            self.order.retain(|b| slots.get(b).is_some_and(|s| !s.dirty));
        }
    }
}

/// The cache as the bcachefs crate reads it: shared, since the volume reads
/// and writes files' data past the same cache its btree lives in. The crate
/// is synchronous, so each request is driven to its end here.
pub struct Shared<D>(pub std::rc::Rc<Cache<D>>);

impl<D> Clone for Shared<D> {
    fn clone(&self) -> Self {
        Self(std::rc::Rc::clone(&self.0))
    }
}

impl<D: Disk> bcachefs::BlockIO for Shared<D> {
    fn read_block(&self, block: bcachefs::BlockNum, buf: &mut bcachefs::BlockBuf) -> Result<(), bcachefs::DeviceError> {
        let out = std::slice::from_mut(buf.as_bytes_mut());
        self.0.block_on(self.0.read(block.raw(), out)).map_err(|_| bcachefs::DeviceError::classify(&Attempted))
    }

    fn write_block(&self, block: bcachefs::BlockNum, buf: &bcachefs::BlockBuf) -> Result<(), bcachefs::DeviceError> {
        let data = std::slice::from_ref(buf.as_bytes());
        self.0.block_on(self.0.write(block.raw(), data)).map_err(|_| bcachefs::DeviceError::classify(&Attempted))
    }

    fn block_count(&self) -> u64 {
        self.0.blocks()
    }

    fn sync(&self) -> Result<(), bcachefs::DeviceError> {
        self.0.block_on(self.0.flush()).map_err(|_| bcachefs::DeviceError::classify(&Attempted))
    }
}

/// What every device [`Error`] is to bcachefs: attempted, since nothing in
/// this server refuses on a clock.
struct Attempted;

impl bcachefs::TransferError for Attempted {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future::poll_fn;
    use std::task::{Poll, Waker};

    use super::*;
    use crate::ram::Ram;
    use toyos_blockdev::{block_on_ready, Executor};

    /// The clean blocks every cache here keeps.
    const CLEAN_LIMIT: usize = 4096;

    /// A disk that counts what reaches it, and refuses its flushes while
    /// `refusing_flush` is set.
    struct Counting {
        ram: Ram,
        reads: Cell<u32>,
        writes: Cell<u32>,
        flushes: Cell<u32>,
        refusing_flush: Cell<bool>,
    }

    impl Counting {
        fn new(blocks: u64) -> Self {
            Self {
                ram: Ram::new(blocks),
                reads: Cell::new(0),
                writes: Cell::new(0),
                flushes: Cell::new(0),
                refusing_flush: Cell::new(false),
            }
        }
    }

    impl BlockDev for Counting {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
            self.reads.set(self.reads.get() + 1);
            self.ram.read(lba, out).await
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
            self.writes.set(self.writes.get() + 1);
            self.ram.write(lba, data).await
        }
        async fn flush(&self) -> Result<(), Error> {
            self.flushes.set(self.flushes.get() + 1);
            if self.refusing_flush.get() {
                return Err(Error::Device);
            }
            self.ram.flush().await
        }
    }

    impl Disk for Counting {}

    fn cache() -> Cache<Counting> {
        Cache::new(Counting::new(1024), CLEAN_LIMIT)
    }

    fn read_one<D: Disk>(c: &Cache<D>, block: u64) -> Result<Block, Error> {
        let mut out = [[0; BLOCK]];
        block_on_ready(c.read(block, &mut out))?;
        Ok(out[0])
    }

    #[test]
    fn a_write_reaches_the_disk_at_the_flush_and_not_before() {
        let c = cache();
        block_on_ready(c.write(7, &[[0xAB; BLOCK]])).unwrap();
        assert_eq!(c.disk.writes.get(), 0);
        assert_eq!(read_one(&c, 7), Ok([0xAB; BLOCK]), "a dirty block is served from the cache");
        block_on_ready(c.flush()).unwrap();
        assert_eq!((c.disk.writes.get(), c.disk.flushes.get(), c.counts().dirty), (1, 1, 0));
        let mut on_disk = [[0; BLOCK]];
        block_on_ready(c.disk.ram.read(7, &mut on_disk)).unwrap();
        assert_eq!(on_disk, [[0xAB; BLOCK]]);
    }

    /// A file's data passes the cache by: read and written on the disk, kept
    /// nowhere, and a write still made durable by the next flush.
    #[test]
    fn data_passes_the_cache_by_and_the_next_flush_keeps_it() {
        let c = cache();
        block_on_ready(c.flush()).unwrap();
        block_on_ready(c.write_data(9, &[[5; BLOCK], [6; BLOCK]])).unwrap();
        assert_eq!((c.disk.writes.get(), c.counts().cached), (1, 0), "on the disk at once, in one request");
        let mut out = [[0; BLOCK]; 2];
        block_on_ready(c.read_data(9, &mut out)).unwrap();
        assert_eq!(out, [[5; BLOCK], [6; BLOCK]]);
        assert_eq!(c.counts().cached, 0, "a data read keeps nothing");
        block_on_ready(c.flush()).unwrap();
        assert_eq!(c.disk.flushes.get(), 2, "the flush asks the disk to keep what passed it by");
    }

    /// A block the cache holds is the newest copy, so data meets it there.
    #[test]
    fn data_meets_a_block_the_cache_holds_in_the_cache() {
        let c = cache();
        block_on_ready(c.write(3, &[[1; BLOCK]])).unwrap();
        let mut out = [[0; BLOCK]; 2];
        block_on_ready(c.read_data(3, &mut out)).unwrap();
        assert_eq!(out[0], [1; BLOCK], "the dirty copy, not the disk's");
        block_on_ready(c.write_data(3, &[[2; BLOCK]])).unwrap();
        assert_eq!((c.disk.writes.get(), c.counts().dirty), (0, 1), "written into the held block");
        assert_eq!(read_one(&c, 3), Ok([2; BLOCK]));
    }

    /// The disk is asked to flush only when the cache wrote it since the last
    /// flush it answered, and at the first flush, which answers for whatever
    /// it held when the cache was made.
    #[test]
    fn a_flush_asks_the_disk_to_flush_only_what_it_wrote_since_its_last() {
        let c = cache();
        let flushed = |c: &Cache<Counting>| {
            block_on_ready(c.flush()).unwrap();
            (c.disk.writes.get(), c.disk.flushes.get())
        };
        assert_eq!(flushed(&c), (0, 1), "the first flush asks the disk");
        assert_eq!(flushed(&c), (0, 1), "nothing written since");
        block_on_ready(c.write(7, &[[1; BLOCK]])).unwrap();
        assert_eq!(flushed(&c), (1, 2));
        assert_eq!(flushed(&c), (1, 2));

        block_on_ready(c.write(8, &[[2; BLOCK]])).unwrap();
        c.disk.refusing_flush.set(true);
        assert_eq!(block_on_ready(c.flush()), Err(Error::Device));
        assert_eq!((c.disk.writes.get(), c.disk.flushes.get(), c.counts().dirty), (2, 3, 0));
        c.disk.refusing_flush.set(false);
        assert_eq!(flushed(&c), (2, 4), "a refused flush is owed with no block dirty");
        assert_eq!(flushed(&c), (2, 4));
    }

    #[test]
    fn contiguous_misses_are_one_request_and_a_second_read_is_none() {
        let c = cache();
        let mut out = vec![[0; BLOCK]; 10];
        block_on_ready(c.read(100, &mut out)).unwrap();
        assert_eq!(c.disk.reads.get(), 1);
        block_on_ready(c.read(100, &mut out)).unwrap();
        assert_eq!(c.disk.reads.get(), 1);
        assert_eq!(c.counts().hits, 10);
    }

    #[test]
    fn dirty_runs_go_out_in_runs() {
        let c = cache();
        for b in [3u64, 4, 5, 9, 10] {
            block_on_ready(c.write(b, &[[b as u8; BLOCK]])).unwrap();
        }
        block_on_ready(c.flush()).unwrap();
        assert_eq!(c.disk.writes.get(), 2, "3..=5 and 9..=10");
    }

    #[test]
    fn a_transfer_past_the_disk_is_refused() {
        let c = cache();
        assert_eq!(block_on_ready(c.write(1024, &[[0; BLOCK]])), Err(Error::Range));
        assert_eq!(read_one(&c, 1024), Err(Error::Range));
        assert_eq!(c.counts().dirty, 0);
    }

    #[test]
    fn clean_blocks_are_bounded_and_dirty_ones_are_kept() {
        let c = Cache::new(Counting::new((CLEAN_LIMIT + 64) as u64), CLEAN_LIMIT);
        block_on_ready(c.write(0, &[[1; BLOCK]])).unwrap();
        for b in 1..(CLEAN_LIMIT as u64 + 32) {
            read_one(&c, b).unwrap();
        }
        let counts = c.counts();
        assert!(counts.cached - counts.dirty <= CLEAN_LIMIT);
        assert_eq!(counts.dirty, 1);
        assert_eq!(read_one(&c, 0), Ok([1; BLOCK]));
    }

    /// Block `b`'s bytes: its index first, so a block read back from anywhere
    /// else is seen, and the rest varying with the offset in it.
    fn pattern(b: u64) -> Block {
        let mut data = [0u8; BLOCK];
        data[..8].copy_from_slice(&b.to_le_bytes());
        for (i, byte) in data.iter_mut().enumerate().skip(8) {
            *byte = (b as usize ^ i.wrapping_mul(7)) as u8;
        }
        data
    }

    #[test]
    fn every_evicted_clean_block_reads_back_off_the_disk_byte_for_byte() {
        let blocks = (CLEAN_LIMIT + 2048) as u64;
        let c = Cache::new(Counting::new(blocks), CLEAN_LIMIT);
        for b in 0..blocks {
            block_on_ready(c.write(b, &[pattern(b)])).unwrap();
        }
        block_on_ready(c.flush()).unwrap();
        assert_eq!(c.counts().dirty, 0, "a flushed cache holds no dirty block");
        for b in 0..blocks {
            assert!(read_one(&c, b) == Ok(pattern(b)), "block {b} read back other bytes than were written");
        }
        assert!(c.disk.reads.get() > 0, "no block was evicted, so none came back off the disk");
    }

    /// A disk that takes writes until it is told to refuse them.
    struct Refusing {
        ram: Ram,
        refusing: Cell<bool>,
    }

    impl BlockDev for Refusing {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
            self.ram.read(lba, out).await
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
            match self.refusing.get() {
                true => Err(Error::Device),
                false => self.ram.write(lba, data).await,
            }
        }
        async fn flush(&self) -> Result<(), Error> {
            match self.refusing.get() {
                true => Err(Error::Device),
                false => self.ram.flush().await,
            }
        }
    }

    impl Disk for Refusing {}

    #[test]
    fn at_the_dirty_limit_a_refused_flush_refuses_the_write_and_the_cache_grows_no_larger() {
        let limit = DIRTY_LIMIT as u64;
        let c = Cache::new(Refusing { ram: Ram::new(limit + 16), refusing: Cell::new(false) }, CLEAN_LIMIT);
        for b in 0..limit {
            block_on_ready(c.write(b, &[[1; BLOCK]])).unwrap();
        }
        c.disk.refusing.set(true);
        let held = c.counts();
        assert_eq!(held.dirty, DIRTY_LIMIT);
        for b in limit..limit + 3 {
            assert_eq!(block_on_ready(c.write(b, &[[2; BLOCK]])), Err(Error::Device), "block {b}");
        }
        assert_eq!(c.counts(), held, "a refused write left the cache no larger");
        block_on_ready(c.write(0, &[[3; BLOCK]])).expect("a block already dirty is rewritten in place");
        assert_eq!(c.counts(), held);
    }

    /// A disk whose every request is pending until the test lets it through,
    /// so another task can run while one is on the wire. It counts each
    /// request that went out while another for one of its blocks was in
    /// flight and either was a write: what the block service's client
    /// forbids.
    struct Held {
        ram: Ram,
        open: Cell<bool>,
        waiting: RefCell<Vec<Waker>>,
        /// `(first, count, write)` of each request in flight.
        flight: RefCell<Vec<(u64, u64, bool)>>,
        overlaps: Cell<u32>,
        /// What [`Disk::depth`] answers.
        depth: Cell<usize>,
        /// Every write is refused while this is set.
        refusing: Cell<bool>,
        /// Requests the closed gate still lets through, one each.
        passes: Cell<u32>,
        writes: Cell<u32>,
        /// Every block handed to [`Disk::write_shared`], as handed.
        kept: RefCell<Vec<(u64, SharedBlock)>>,
    }

    impl Held {
        fn new(blocks: u64) -> Self {
            Self {
                ram: Ram::new(blocks),
                open: Cell::new(true),
                waiting: RefCell::default(),
                flight: RefCell::default(),
                overlaps: Cell::new(0),
                depth: Cell::new(1),
                refusing: Cell::new(false),
                passes: Cell::new(0),
                writes: Cell::new(0),
                kept: RefCell::default(),
            }
        }

        async fn gate(&self) {
            poll_fn(|cx| {
                if self.open.get() {
                    return Poll::Ready(());
                }
                if self.passes.get() > 0 {
                    self.passes.set(self.passes.get() - 1);
                    return Poll::Ready(());
                }
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            })
            .await
        }

        /// Let one request waiting at the gate through, the first polled.
        fn release_one(&self) {
            self.passes.set(self.passes.get() + 1);
            self.waiting.take().into_iter().for_each(Waker::wake);
        }

        fn release(&self) {
            self.open.set(true);
            self.waiting.take().into_iter().for_each(Waker::wake);
        }

        fn send(&self, first: u64, count: usize, write: bool) {
            let end = first + count as u64;
            let mut flight = self.flight.borrow_mut();
            if flight.iter().any(|&(f, n, w)| (w || write) && f < end && first < f + n) {
                self.overlaps.set(self.overlaps.get() + 1);
            }
            flight.push((first, count as u64, write));
        }

        fn landed(&self, first: u64, write: bool) {
            let mut flight = self.flight.borrow_mut();
            let at = flight.iter().position(|&(f, _, w)| f == first && w == write).expect("a request in flight");
            flight.remove(at);
        }

        fn writes_in_flight(&self) -> usize {
            self.flight.borrow().iter().filter(|r| r.2).count()
        }
    }

    impl BlockDev for Held {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        /// What the disk holds when the read is asked, answered when it is
        /// let through.
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
            self.send(lba, out.len(), false);
            self.ram.read(lba, out).await?;
            self.gate().await;
            self.landed(lba, false);
            Ok(())
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
            self.writes.set(self.writes.get() + 1);
            self.send(lba, data.len(), true);
            self.gate().await;
            self.landed(lba, true);
            if self.refusing.get() {
                return Err(Error::Device);
            }
            self.ram.write(lba, data).await
        }
        async fn flush(&self) -> Result<(), Error> {
            self.gate().await;
            Ok(())
        }
    }

    impl Disk for Held {
        fn depth(&self) -> usize {
            self.depth.get()
        }

        async fn write_shared(&self, lba: u64, blocks: &[SharedBlock]) -> Result<(), Error> {
            self.kept.borrow_mut().extend((lba..).zip(blocks.iter().cloned()));
            let data: Vec<Block> = blocks.iter().map(|block| ***block).collect();
            self.write(lba, &data).await
        }
    }

    /// Blocks 1, 3, 5, … written, one run each: `runs` of them.
    fn runs_apart(c: &Cache<Held>, runs: u64) {
        for k in 0..runs {
            block_on_ready(c.write(2 * k + 1, &[[k as u8 + 1; BLOCK]])).unwrap();
        }
    }

    /// A flush keeps as many runs in flight as the disk takes, and no more.
    #[test]
    fn a_flush_keeps_as_many_runs_in_flight_as_the_disk_takes() {
        let c = Cache::new(Held::new(16), CLEAN_LIMIT);
        c.disk.depth.set(3);
        runs_apart(&c, 5);
        c.disk.open.set(false);
        let exec = Executor::new();
        exec.spawn(async { c.flush().await.unwrap() });
        exec.run_until_stalled();
        assert_eq!(c.disk.writes_in_flight(), 3);
        c.disk.release();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(c.disk.overlaps.get(), 0);
        assert_eq!(c.counts().dirty, 0);
        for k in 0..5 {
            assert_eq!(on_disk(&c.disk, 2 * k + 1), [k as u8 + 1; BLOCK]);
        }
    }

    /// The disk is handed the cache's own blocks, not copies of them.
    #[test]
    fn a_flush_shares_the_cache_s_blocks_with_the_disk() {
        let c = Cache::new(Held::new(16), CLEAN_LIMIT);
        block_on_ready(c.write(3, &[[1; BLOCK]])).unwrap();
        block_on_ready(c.flush()).unwrap();
        let kept = c.disk.kept.borrow();
        let inner = c.inner.borrow();
        assert_eq!(kept.len(), 1);
        assert!(Rc::ptr_eq(&kept[0].1, &inner.slots[&3].data), "the disk was handed a copy");
    }

    /// Once a run is refused no other is begun, though another is still in
    /// flight, and every block not written stays dirty for the next flush.
    #[test]
    fn a_refused_run_begins_no_other_and_leaves_the_rest_dirty() {
        let c = Cache::new(Held::new(16), CLEAN_LIMIT);
        c.disk.depth.set(2);
        runs_apart(&c, 4);
        c.disk.refusing.set(true);
        c.disk.open.set(false);
        let exec = Executor::new();
        exec.spawn(async { assert_eq!(c.flush().await, Err(Error::Device)) });
        exec.run_until_stalled();
        assert_eq!(c.disk.writes_in_flight(), 2);
        c.disk.release_one();
        exec.run_until_stalled();
        assert_eq!(c.disk.writes_in_flight(), 1, "the refused run's place was taken");
        c.disk.release();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(c.disk.writes.get(), 2, "a run was begun after one was refused");
        assert_eq!(c.counts().dirty, 4);
        c.disk.refusing.set(false);
        block_on_ready(c.flush()).unwrap();
        assert_eq!(c.counts().dirty, 0);
    }

    /// What block `block` holds on the disk.
    fn on_disk(disk: &Held, block: u64) -> Block {
        let mut out = [[0; BLOCK]];
        block_on_ready(disk.ram.read(block, &mut out)).unwrap();
        out[0]
    }

    #[test]
    fn a_block_written_while_a_flush_carries_its_older_copy_stays_dirty() {
        let c = Cache::new(Held::new(16), CLEAN_LIMIT);
        block_on_ready(c.write(3, &[[1; BLOCK]])).unwrap();
        c.disk.open.set(false);
        let exec = Executor::new();
        exec.spawn(async { c.flush().await.unwrap() });
        assert_eq!(exec.run_until_stalled(), 1, "the flush's write is on the wire");
        block_on_ready(c.write(3, &[[2; BLOCK]])).unwrap();
        c.disk.release();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(c.counts().dirty, 1, "the second write was not on the disk the flush made durable");
        block_on_ready(c.flush()).unwrap();
        assert_eq!(on_disk(&c.disk, 3), [2; BLOCK]);
    }

    /// A second flush asked while the first is writing waits for it, then
    /// writes the block's newer copy: never both copies in flight at once,
    /// whose landing order nothing fixes.
    #[test]
    fn a_flush_asked_during_another_waits_and_then_writes_the_newer_copy() {
        let c = Cache::new(Held::new(16), CLEAN_LIMIT);
        block_on_ready(c.write(3, &[[1; BLOCK]])).unwrap();
        c.disk.open.set(false);
        let exec = Executor::new();
        exec.spawn(async { c.flush().await.unwrap() });
        assert_eq!(exec.run_until_stalled(), 1, "the first flush's write is on the wire");
        block_on_ready(c.write(3, &[[2; BLOCK]])).unwrap();
        exec.spawn(async { c.flush().await.unwrap() });
        assert_eq!(exec.run_until_stalled(), 2);
        assert_eq!(c.disk.writes_in_flight(), 1, "the second flush wrote beside the first");
        c.disk.release();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(c.disk.overlaps.get(), 0, "two requests for one block were in flight with a write");
        assert_eq!(on_disk(&c.disk, 3), [2; BLOCK]);
        assert_eq!(c.counts().dirty, 0);
    }

    /// A block fetched, then written into the cache and flushed while the
    /// fetch is out: the flush waits for the fetch before it writes the
    /// block, and the cache keeps the write's copy, not the fetch's.
    #[test]
    fn a_flush_waits_for_a_fetch_of_its_block_and_the_write_is_kept() {
        let c = Cache::new(Held::new(16), CLEAN_LIMIT);
        block_on_ready(c.disk.ram.write(5, &[[1; BLOCK]])).unwrap();
        c.disk.open.set(false);
        let exec = Executor::new();
        exec.spawn(async {
            let mut out = [[0; BLOCK]];
            c.read(5, &mut out).await.unwrap();
        });
        assert_eq!(exec.run_until_stalled(), 1, "the fetch is on the wire with the disk's old copy");
        block_on_ready(c.write(5, &[[2; BLOCK]])).unwrap();
        exec.spawn(async { c.flush().await.unwrap() });
        assert_eq!(exec.run_until_stalled(), 2);
        assert_eq!(c.disk.writes_in_flight(), 0, "the flush wrote the block under its fetch");
        c.disk.release();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(c.disk.overlaps.get(), 0, "a fetch and a write of one block were in flight together");
        assert_eq!(on_disk(&c.disk, 5), [2; BLOCK]);
        assert_eq!(read_one(&c, 5), Ok([2; BLOCK]));
    }
}
