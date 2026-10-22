//! The one cache every block of a volume passes through: a filesystem's
//! metadata and its files' data alike. It is a [`BlockDev`] over the disk.
//!
//! **A write lands here and reaches the disk at a flush.** A flush writes
//! every dirty block, lowest first and in runs, then asks the disk to make
//! them durable; a volume's sync is its own metadata written into the cache
//! and then this. A write that would take the cache past [`DIRTY_LIMIT`] dirty
//! blocks flushes first, and is refused whole when that flush is, so memory
//! bounds the write-back and not the other way round.
//!
//! **Clean blocks are kept up to [`CLEAN_LIMIT`]** and the oldest-read goes
//! first; a dirty block is never dropped.
//!
//! **Several tasks may use it at once.** No borrow of its state is held
//! across a disk request, so what a request answers is checked against what
//! changed meanwhile: a block written while a flush carried an older copy
//! stays dirty, and a fetch a write overtook is not kept.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;

use toyos_blockdev::{within, Block, BlockDev, Error, BLOCK};

/// A disk this server owns, and what to do when nothing can go on until it
/// answers.
pub trait Disk: BlockDev {
    /// Returns once a request asked of this disk may have been answered. A
    /// disk that answers every request as it is asked is never waited on.
    fn wait(&self) {
        panic!("fileserver: a disk that answers as it is asked was waited on");
    }
}

impl Disk for diskserver::disk::Served {
    fn wait(&self) {
        diskserver::disk::Served::wait(self)
    }
}

/// Clean blocks kept: 64 MiB.
pub const CLEAN_LIMIT: usize = 16 * 1024;

/// Dirty blocks held before the cache flushes by itself: 32 MiB.
pub const DIRTY_LIMIT: usize = 8 * 1024;

/// The longest run one disk request carries, in blocks.
const RUN: usize = 32;

struct Slot {
    data: Box<Block>,
    dirty: bool,
    /// Which write put `data` here, so a flush that carried an older copy
    /// leaves the block dirty.
    version: u64,
}

/// Blocks being read from the disk into the cache.
struct Fetch {
    first: u64,
    count: usize,
    /// A write reached one of them since the read was asked, so what it
    /// answers may be older than the cache or the disk holds.
    overtaken: bool,
}

#[derive(Default)]
struct Inner {
    slots: BTreeMap<u64, Slot>,
    /// Clean blocks in the order they were read or cleaned; stale entries are
    /// skipped when popped.
    order: VecDeque<u64>,
    dirty: usize,
    versions: u64,
    fetches: BTreeMap<u64, Fetch>,
    fetched: u64,
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
    pub fn new(disk: D) -> Self {
        Self { disk, inner: RefCell::default() }
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

    /// Blocks `first..first + count`, each handed to `put` with its index from
    /// `first`, out of the cache itself: a block that is not there is fetched
    /// into it first, in runs, so a read of cached blocks copies each once.
    pub async fn visit(&self, first: u64, count: usize, mut put: impl FnMut(usize, &Block)) -> Result<(), Error> {
        within(first, count, self.disk.blocks())?;
        let mut i = 0;
        while i < count {
            let block = first + i as u64;
            let (run, id) = {
                let mut inner = self.inner.borrow_mut();
                inner.reads += 1;
                if let Some(slot) = inner.slots.get(&block) {
                    put(i, &slot.data);
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
                inner.fetches.insert(id, Fetch { first: block, count: run, overtaken: false });
                (run, id)
            };
            let mut span: Vec<Block> = vec![[0; BLOCK]; run];
            let read = self.disk.read(block, &mut span).await;
            let mut inner = self.inner.borrow_mut();
            let fetch = inner.fetches.remove(&id).expect("fileserver: a fetch outlived its record");
            read?;
            for (k, data) in span.iter().enumerate() {
                let at = block + k as u64;
                // A block that arrived while this was asked is newer than
                // what the disk answered, and so is one a write overtook.
                match inner.slots.get(&at) {
                    Some(slot) => put(i + k, &slot.data),
                    None => {
                        put(i + k, data);
                        if !fetch.overtaken {
                            inner.slots.insert(at, Slot { data: Box::new(*data), dirty: false, version: 0 });
                            inner.order.push_back(at);
                        }
                    }
                }
            }
            i += run;
        }
        self.inner.borrow_mut().evict();
        Ok(())
    }
}

impl<D: Disk> BlockDev for Cache<D> {
    fn blocks(&self) -> u64 {
        self.disk.blocks()
    }

    /// Each block from the cache where it is there and in runs from the disk
    /// where it is not.
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.visit(lba, out.len(), |k, block| out[k] = *block).await
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
        inner.overtake(lba, data.len());
        for (block, chunk) in (lba..).zip(data) {
            inner.versions += 1;
            let version = inner.versions;
            let newly = match inner.slots.get_mut(&block) {
                Some(slot) => {
                    *slot.data = *chunk;
                    slot.version = version;
                    !std::mem::replace(&mut slot.dirty, true)
                }
                None => {
                    inner.slots.insert(block, Slot { data: Box::new(*chunk), dirty: true, version });
                    true
                }
            };
            if newly {
                inner.dirty += 1;
            }
        }
        Ok(())
    }

    /// Every dirty block to the disk, lowest first and in runs, then the disk
    /// made durable. A block the disk refused stays dirty.
    async fn flush(&self) -> Result<(), Error> {
        let dirty: Vec<u64> = self.inner.borrow().slots.iter().filter(|(_, s)| s.dirty).map(|(b, _)| *b).collect();
        let mut buf: Vec<Block> = vec![[0; BLOCK]; RUN];
        let mut i = 0;
        while i < dirty.len() {
            let first = dirty[i];
            let mut run = 1;
            while i + run < dirty.len() && run < RUN && dirty[i + run] == first + run as u64 {
                run += 1;
            }
            let versions: Vec<u64> = {
                let mut inner = self.inner.borrow_mut();
                // A fetch beside this write may answer the disk's older copy.
                inner.overtake(first, run);
                (0..run)
                    .map(|k| {
                        let slot = &inner.slots[&(first + k as u64)];
                        buf[k] = *slot.data;
                        slot.version
                    })
                    .collect()
            };
            self.disk.write(first, &buf[..run]).await?;
            let mut inner = self.inner.borrow_mut();
            for (k, version) in versions.into_iter().enumerate() {
                let block = first + k as u64;
                // Cleaned meanwhile by another flush, or written again.
                if let Some(slot) = inner.slots.get_mut(&block)
                    && slot.dirty
                    && slot.version == version
                {
                    slot.dirty = false;
                    inner.dirty -= 1;
                    inner.order.push_back(block);
                }
            }
            i += run;
        }
        self.disk.flush().await?;
        self.inner.borrow_mut().evict();
        Ok(())
    }
}

impl Inner {
    /// Every fetch of a block in `first..first + count` overtaken.
    fn overtake(&mut self, first: u64, count: usize) {
        let end = first + count as u64;
        for fetch in self.fetches.values_mut() {
            if fetch.first < end && first < fetch.first + fetch.count as u64 {
                fetch.overtaken = true;
            }
        }
    }

    /// Drop the oldest clean blocks past [`CLEAN_LIMIT`].
    fn evict(&mut self) {
        while self.slots.len() - self.dirty > CLEAN_LIMIT {
            let Some(block) = self.order.pop_front() else { break };
            if self.slots.get(&block).is_some_and(|s| !s.dirty) {
                self.slots.remove(&block);
            }
        }
        // A queue of mostly stale entries is compacted, so it stays the size
        // of what it orders.
        if self.order.len() > 4 * (CLEAN_LIMIT + DIRTY_LIMIT) {
            let slots = &self.slots;
            self.order.retain(|b| slots.get(b).is_some_and(|s| !s.dirty));
        }
    }
}

/// The cache as the bcachefs crate reads it: shared, since the volume reads
/// files' data through the same cache its btree lives in. The crate is
/// synchronous, so each request is driven to its end here.
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

    /// A disk that counts what reaches it.
    struct Counting {
        ram: Ram,
        reads: Cell<u32>,
        writes: Cell<u32>,
        flushes: Cell<u32>,
    }

    impl Counting {
        fn new(blocks: u64) -> Self {
            Self { ram: Ram::new(blocks), reads: Cell::new(0), writes: Cell::new(0), flushes: Cell::new(0) }
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
            self.ram.flush().await
        }
    }

    impl Disk for Counting {}

    fn cache() -> Cache<Counting> {
        Cache::new(Counting::new(1024))
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
        let c = Cache::new(Counting::new((CLEAN_LIMIT + 64) as u64));
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
        let c = Cache::new(Counting::new(blocks));
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
        let c = Cache::new(Refusing { ram: Ram::new(limit + 16), refusing: Cell::new(false) });
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
    /// so another task can run while one is on the wire.
    struct Held {
        ram: Ram,
        open: Cell<bool>,
        waiting: RefCell<Vec<Waker>>,
    }

    impl Held {
        fn new(blocks: u64) -> Self {
            Self { ram: Ram::new(blocks), open: Cell::new(true), waiting: RefCell::default() }
        }

        async fn gate(&self) {
            poll_fn(|cx| match self.open.get() {
                true => Poll::Ready(()),
                false => {
                    self.waiting.borrow_mut().push(cx.waker().clone());
                    Poll::Pending
                }
            })
            .await
        }

        fn release(&self) {
            self.open.set(true);
            self.waiting.take().into_iter().for_each(Waker::wake);
        }
    }

    impl BlockDev for Held {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        /// What the disk holds when the read is asked, answered when it is
        /// let through.
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
            self.ram.read(lba, out).await?;
            self.gate().await;
            Ok(())
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
            self.gate().await;
            self.ram.write(lba, data).await
        }
        async fn flush(&self) -> Result<(), Error> {
            self.gate().await;
            Ok(())
        }
    }

    impl Disk for Held {}

    #[test]
    fn a_block_written_while_a_flush_carries_its_older_copy_stays_dirty() {
        let c = Cache::new(Held::new(16));
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
        let mut on_disk = [[0; BLOCK]];
        block_on_ready(c.disk.ram.read(3, &mut on_disk)).unwrap();
        assert_eq!(on_disk, [[2; BLOCK]]);
    }

    #[test]
    fn a_fetch_a_write_overtook_is_not_kept() {
        let c = Cache::new(Held::new(16));
        block_on_ready(c.disk.ram.write(5, &[[1; BLOCK]])).unwrap();
        c.disk.open.set(false);
        let exec = Executor::new();
        exec.spawn(async {
            let mut out = [[0; BLOCK]];
            c.read(5, &mut out).await.unwrap();
        });
        assert_eq!(exec.run_until_stalled(), 1, "the fetch is on the wire with the disk's old copy");
        // Written, made durable and dropped while the fetch is out.
        c.disk.open.set(true);
        block_on_ready(c.write(5, &[[2; BLOCK]])).unwrap();
        block_on_ready(c.flush()).unwrap();
        c.inner.borrow_mut().slots.remove(&5);
        c.disk.release();
        assert_eq!(exec.run_until_stalled(), 0);
        assert_eq!(read_one(&c, 5), Ok([2; BLOCK]), "the fetch's older copy was kept over the write");
    }
}
