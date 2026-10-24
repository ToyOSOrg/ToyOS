//! The bucket lifecycle on a mounted volume: an emptied bucket reused once
//! its emptying is durable and no traversal may still lead into it, and a
//! volume out of room refusing changes while it can still remove names and
//! unmount clean.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::vec::Vec;

use toyos_blockdev::executor::Executor;
use toyos_blockdev::{block_on_ready, Block, BlockDev, Bytes, BLOCK};

use super::mount::{random, MEMORY, NOW, OWNER};
use super::*;

/// A device whose blocks read zero until written, holding only those that
/// are not: a volume of upstream's large geometry at little memory.
pub(super) struct Sparse {
    blocks: u64,
    written: RefCell<BTreeMap<u64, Block>>,
}

impl Sparse {
    pub(super) fn new(bytes: u64) -> Self {
        Sparse { blocks: bytes / BLOCK as u64, written: RefCell::default() }
    }
}

impl BlockDev for Sparse {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        toyos_blockdev::within(lba, out.len(), self.blocks)?;
        let written = self.written.borrow();
        for (i, block) in out.iter_mut().enumerate() {
            *block = written.get(&(lba + i as u64)).copied().unwrap_or([0; BLOCK]);
        }
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        toyos_blockdev::within(lba, data.len(), self.blocks)?;
        let mut written = self.written.borrow_mut();
        for (i, block) in data.iter().enumerate() {
            if block.iter().all(|&b| b == 0) {
                written.remove(&(lba + i as u64));
            } else {
                written.insert(lba + i as u64, *block);
            }
        }
        Ok(())
    }

    async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
        Ok(())
    }
}

fn formatted_on<D: BlockDev>(dev: &D) {
    let options = Options { label: b"toyos", time: 1_761_177_600_000_000_000, encryption: None };
    block_on_ready(mkfs(dev, &options, &Tree::new(), &mut Seeded(0x5eed))).unwrap();
}

/// `files` files in the root, then each renamed back and forth until the
/// mount has reused `reuses` buckets, a commit after every `every`; then a
/// clean unmount. How many renames it took.
fn churn<D: BlockDev>(dev: &D, options: MountOptions, files: usize, reuses: usize, every: usize) -> usize {
    let m = block_on_ready(Mount::open(dev, options, random())).unwrap();
    let stalled = || panic!("stalled; {}", m.dump());
    let renames = Executor::new().block_on(
        async {
            let r = m.root();
            for i in 0..files {
                m.create(r, format!("a{i}").as_bytes(), New::File, OWNER, NOW).await.unwrap();
            }
            let mut n = 0;
            while m.reused() < reuses {
                let (i, round) = (n % files, n / files);
                let (from, to) = if round % 2 == 0 { ("a", "b") } else { ("b", "a") };
                m.rename((r, format!("{from}{i}").as_bytes()), (r, format!("{to}{i}").as_bytes()), Rename::NoReplace, NOW).await.unwrap();
                n += 1;
                if n % every == 0 {
                    m.commit().await.unwrap();
                }
                assert!(n < 100_000, "{} buckets reused in {n} renames", m.reused());
            }
            n
        },
        stalled,
    );
    block_on_ready(m.unmount()).unwrap();
    renames
}

/// Renames rewrite the same leaves, each rewrite emptying the bucket of the
/// node it replaces: on a volume of 2 048 buckets, with reclaim writing
/// before every rename, more buckets are taken than it has, and none ever
/// runs out. Red when an emptied bucket is never taken again: the volume
/// answers `NoSpace`.
#[test]
fn renames_past_the_volumes_buckets_reuse_them_and_never_run_out() {
    let dev = formatted(64 << 20, None);
    let nbuckets = block_on_ready(Volume::open(&dev)).unwrap().superblock().member.nbuckets as usize;
    churn(&dev, super::crash::PRESSED, 40, nbuckets, 25);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(names(&v, v.root()).len(), 40);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// Upstream's geometry for a large device: 2 MiB buckets of eight 256 KiB
/// nodes. Partly filled buckets carry fragmentation LRU entries, the open
/// bucket is filled slot by slot across rounds, and a bucket is reused only
/// once all eight of its nodes are gone and that is durable.
#[test]
fn buckets_of_eight_nodes_fill_slot_by_slot_and_are_reused_once_all_eight_are_gone() {
    let dev = Sparse::new(4 << 30);
    formatted_on(&dev);
    {
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        let sb = v.superblock();
        assert_eq!((sb.member.bucket_sectors, sb.node_sectors()), (4096, 512), "2 MiB buckets of 256 KiB nodes");
    }
    churn(&dev, super::crash::PRESSED, 200, 4, 50);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(v.readdir(v.root(), 0, 1000)).unwrap().0.len(), 200);
    let mut lru = 0;
    block_on_ready(super::read::Source::walk(&v, Btree::Lru, Bpos::MIN, |_, _| {
        lru += 1;
        Ok(true)
    }))
    .unwrap();
    assert!(lru > 0, "partly filled btree buckets on the fragmentation LRU");
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// Empty buckets as `v`'s device accounting counts them: free and
/// need_discard (A7.12).
fn empty_buckets<D: BlockDev>(v: &Volume<D>) -> u64 {
    [super::value::data_type::FREE, super::value::data_type::NEED_DISCARD]
        .iter()
        .map(|&t| {
            let pos = super::value::accounting_pos(&[super::value::class::DEV_DATA_TYPE, 0, t]);
            block_on_ready(super::read::Source::get(v, Btree::Accounting, pos)).unwrap().map_or(0, |(_, v)| u64::from_le_bytes(v[..8].try_into().unwrap()))
        })
        .sum()
}

/// A 64 MiB volume whose one file leaves about `free` buckets empty.
pub(super) fn nearly_full(data: &mut Vec<u8>, free: u64) -> Image {
    let v = block_on_ready(Volume::open(formatted(64 << 20, None))).unwrap();
    let (empty, bucket) = (empty_buckets(&v), u64::from(v.superblock().member.bucket_sectors) * SECTOR as u64);
    data.resize(((empty - free) * bucket) as usize * 63 / 64, 7);
    let mut tree = Tree::new();
    tree.file(Tree::ROOT, b"big", data, Meta { perm: 0o644, uid: 0, gid: 0 }).unwrap();
    let dev = populated(64 << 20, &tree, 9);
    let left = empty_buckets(&block_on_ready(Volume::open(&dev)).unwrap());
    assert!(left.abs_diff(free) < free / 4, "{left} buckets empty, not about {free}");
    dev
}

/// Creates until the volume is out of room: the refusal is admission's, by
/// name, before reclaim could find no bucket, and the mount keeps writing.
/// Names are still removed, an orphan still released, everything commits,
/// and the volume unmounts clean. Red when admission ignores the room left:
/// reclaim then finds no bucket and stops the mount; and red when removals
/// leave no reserve: the unlink answers `NoSpace`.
#[test]
fn a_full_volume_refuses_changes_by_name_and_still_removes_names_and_unmounts_clean() {
    let mut data = Vec::new();
    let dev = nearly_full(&mut data, 400);
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let stalled = || panic!("stalled; {}", m.dump());
    let made = Executor::new().block_on(
        async {
            let r = m.root();
            let mut n = 0;
            loop {
                match m.create(r, format!("f{n}").as_bytes(), New::File, OWNER, NOW).await {
                    Ok(_) => n += 1,
                    Err(e) => {
                        assert_eq!(e, Error::NoSpace("no room left for the btree nodes a change may write"), "after {n} creates");
                        break;
                    }
                }
                assert!(n < 100_000);
            }
            m.commit().await.unwrap();
            assert_eq!(m.create(r, b"again", New::Dir, OWNER, NOW).await.err(), Some(Error::NoSpace("no room left for the btree nodes a change may write")));
            for i in 0..20 {
                m.unlink(r, format!("f{i}").as_bytes(), false, NOW).await.unwrap();
            }
            let orphan = m.lookup(r, b"f20").await.unwrap().unwrap().ino;
            m.unlink(r, b"f20", false, NOW).await.unwrap();
            m.release(orphan).await.unwrap();
            m.commit().await.unwrap();
            n
        },
        stalled,
    );
    assert!(made > 1_000, "{made} creates filled the volume");
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let (entries, more) = block_on_ready(v.readdir(v.root(), 0, usize::MAX)).unwrap();
    assert_eq!((entries.len(), more), (made + 1 - 21, false), "the file and what was made, less what was removed");
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// The buckets `src`'s need_discard lists, each with the sequence that
/// emptied it.
pub(super) fn need_discard<S: super::read::Source>(src: &S) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    block_on_ready(super::read::Source::walk(src, Btree::NeedDiscard, Bpos::MIN, |k, _| {
        out.push((k.pos.offset, k.pos.inode));
        Ok(true)
    }))
    .unwrap();
    out
}

/// Reclaim's two halves, by hand, until no dirty node is left: every
/// sector it wrote a node at.
pub(super) fn reclaim_all<D: BlockDev>(m: &Mount<D>) -> Vec<u64> {
    let mut sectors = Vec::new();
    loop {
        let plans = block_on_ready(m.write_batch(super::super::mount::Order::OldestPin)).unwrap();
        if plans.is_empty() {
            return sectors;
        }
        sectors.extend(plans.iter().flat_map(|p| p.sectors(m)));
        block_on_ready(m.point_batch(plans)).unwrap();
    }
}

/// A crash on a full volume, then a read-write mount: its window replays,
/// every node it writes goes to a fresh one (§3.5), and the room left still
/// takes those rewrites. Names are removed, an orphan released, and the
/// volume unmounts clean and checks clean.
#[test]
fn a_full_volume_crashed_mounts_read_write_and_still_removes_names() {
    let mut data = Vec::new();
    let dev = nearly_full(&mut data, 400);
    let made = {
        let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
        let made = Executor::new().block_on(
            async {
                let mut n = 0;
                while m.create(m.root(), format!("f{n}").as_bytes(), New::File, OWNER, NOW).await.is_ok() {
                    n += 1;
                }
                m.commit().await.unwrap();
                n
            },
            || panic!("stalled"),
        );
        // A crash: the mount dropped, never unmounted.
        made
    };
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let stalled = || panic!("stalled; {}", m.dump());
    Executor::new().block_on(
        async {
            let r = m.root();
            assert_eq!(m.create(r, b"again", New::File, OWNER, NOW).await.err(), Some(Error::NoSpace("no room left for the btree nodes a change may write")));
            for i in 0..20 {
                m.unlink(r, format!("f{i}").as_bytes(), false, NOW).await.unwrap();
            }
            let orphan = m.lookup(r, b"f20").await.unwrap().unwrap().ino;
            m.unlink(r, b"f20", false, NOW).await.unwrap();
            m.release(orphan).await.unwrap();
            m.commit().await.unwrap();
        },
        stalled,
    );
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let (entries, more) = block_on_ready(v.readdir(v.root(), 0, usize::MAX)).unwrap();
    assert_eq!((entries.len(), more), (made + 1 - 21, false));
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// A bucket emptied by an entry not yet durable is not written again: a
/// crash before that entry is on the medium brings back the pointers into
/// it (A6.19). After an unclean mount the first write to each node goes to
/// a fresh node, emptying the bucket the node was in; later batches' new
/// nodes avoid those buckets while that entry is unwritten. The crash then
/// leaves the medium as it is, the entry never written; it reads whole and
/// checks clean. Red when an emptied bucket is taken before its emptying is
/// durable: a later batch writes over a node the medium's pointers lead
/// into.
#[test]
fn a_bucket_emptied_by_an_entry_not_yet_durable_is_not_written_again() {
    let dev = formatted(64 << 20, None);
    let bucket = {
        let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
        block_on_ready(m.create(m.root(), b"a", New::File, OWNER, NOW)).unwrap();
        block_on_ready(m.commit()).unwrap();
        // A crash: the mount dropped, never unmounted.
        u64::from(super::read::Source::superblock(&m).member.bucket_sectors)
    };
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let before = need_discard(&m);
    for i in 0..30 {
        block_on_ready(m.create(m.root(), format!("b{i}").as_bytes(), New::File, OWNER, NOW)).unwrap();
    }
    let written = reclaim_all(&m);
    let emptied: Vec<u64> = need_discard(&m).into_iter().filter(|e| !before.contains(e)).map(|(b, _)| b).collect();
    let crashed = Bytes::new(dev.get().clone());
    drop(m);
    let v = block_on_ready(Volume::open(&crashed)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)), Ok(Vec::new()));
    assert_eq!(names(&v, v.root()), ["a"]);
    assert!(emptied.len() >= 4, "the fresh rewrites emptied {} buckets", emptied.len());
    assert!(written.iter().all(|s| !emptied.contains(&(s / bucket))), "nodes written into buckets emptied by an unwritten entry");
}

/// A device that, once armed, holds its next read of `lba` until opened: a
/// traversal paused between reading a pointer and reading the node it
/// leads to.
struct Gated<'a> {
    dev: &'a Image,
    lba: u64,
    open: std::cell::Cell<bool>,
    /// Set once the held read is asked, and while unarmed.
    held: std::cell::Cell<bool>,
    waker: RefCell<Option<core::task::Waker>>,
}

impl Gated<'_> {
    fn release(&self) {
        self.open.set(true);
        if let Some(w) = self.waker.borrow_mut().take() {
            w.wake();
        }
    }
}

impl BlockDev for Gated<'_> {
    fn blocks(&self) -> u64 {
        self.dev.blocks()
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        if (lba..lba + out.len() as u64).contains(&self.lba) && !self.held.replace(true) {
            core::future::poll_fn(|cx| {
                if self.open.get() {
                    return core::task::Poll::Ready(());
                }
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                core::task::Poll::Pending
            })
            .await;
        }
        self.dev.read(lba, out).await
    }

    async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.write(lba, data).await
    }

    async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.flush().await
    }
}

/// A listing reads the root of the dirents and, before it reads the leaf
/// that pointer leads to, the leaf is replaced, its bucket's emptying made
/// durable, and more nodes written, which would take that bucket first. No
/// bucket the listing may still lead into is written while it reads, so
/// the leaf it reads is still the one its pointer names, and the listing
/// completes. Its stale cached leaf goes when the bucket is taken after it
/// ends. Red when a bucket is taken while a traversal begun before it
/// emptied still reads: a node is written into the bucket the listing
/// leads into. Red when the stale node is kept: two nodes cached at one
/// sector.
#[test]
fn a_bucket_a_traversal_may_still_lead_into_is_not_written_while_it_reads() {
    let dev = formatted(64 << 20, None);
    let names_made: Vec<Vec<u8>> = (0..1500).map(|i| format!("n{i}").into_bytes()).collect();
    {
        let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
        Executor::new().block_on(
            async {
                for n in &names_made {
                    m.create(m.root(), n, New::File, OWNER, NOW).await.unwrap();
                }
            },
            || panic!("stalled"),
        );
        block_on_ready(m.unmount()).unwrap();
    }
    // The root's first dirents leaf, under an interior root, and a name in it.
    let (leaf, in_leaf, crash) = {
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        assert!(v.roots().iter().any(|r| r.btree == Btree::Dirents && r.level > 0), "dirents under an interior root");
        let (ptr, max) = leaf_of(&v, Btree::Dirents, Bpos::new(v.root(), 0, 0));
        let keys = block_on_ready(v.node(Btree::Dirents, 0, &ptr, max)).unwrap().keys;
        let name = |k: &(super::bkey::Key, Vec<u8>)| super::value::Dirent::decode(&k.1).unwrap().name.to_vec();
        let in_leaf = name(keys.iter().find(|k| k.0.kind == kind::DIRENT).unwrap());
        let past = |n: &[u8]| hash_of(&v, v.root(), n) > max.offset;
        let crash = (0..).map(|i| format!("x{i}").into_bytes()).find(|n| past(n)).unwrap();
        (ptr.ptr.sector, in_leaf, crash)
    };
    // A crash after one more commit, so every node's next write is fresh;
    // its name in another leaf, so the replay leaves this one unread.
    {
        let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
        block_on_ready(m.create(m.root(), &crash, New::File, OWNER, NOW)).unwrap();
        block_on_ready(m.commit()).unwrap();
    }
    let gated = Gated { dev: &dev, lba: leaf * SECTOR as u64 / BLOCK as u64, open: std::cell::Cell::default(), held: true.into(), waker: RefCell::default() };
    let m = block_on_ready(Mount::open(&gated, MEMORY, random())).unwrap();
    let bucket = u64::from(super::read::Source::superblock(&m).member.bucket_sectors);
    gated.held.set(false);
    let listed = std::rc::Rc::new(RefCell::new(None));
    let exec = Executor::new();
    let out = std::rc::Rc::clone(&listed);
    let mm = &m;
    exec.spawn(async move {
        *out.borrow_mut() = Some(mm.readdir(mm.root(), 0, 10_000).await.map(|(e, _)| e.len()));
    });
    exec.run_until_stalled();
    assert!(gated.held.get() && listed.borrow().is_none(), "the listing waits on the leaf's read");
    let stalled = || panic!("stalled; {}", m.dump());
    let leaf_bucket = leaf / bucket;
    exec.block_on(
        async {
            // The leaf replaced: its bucket emptied, and that made durable.
            m.unlink(m.root(), &in_leaf, false, NOW).await.unwrap();
            let mut written = reclaim_all(&m);
            m.commit().await.unwrap();
            assert!(need_discard(&m).iter().any(|&(b, _)| b == leaf_bucket), "the leaf's bucket emptied");
            // More fresh rewrites, each batch taking an emptied bucket first.
            for n in names_made.iter().filter(|n| **n != in_leaf).take(40) {
                m.unlink(m.root(), n, false, NOW).await.unwrap();
                written.extend(reclaim_all(&m));
                m.commit().await.unwrap();
            }
            assert!(m.reused() > 0, "emptied buckets taken again");
            assert!(written.iter().all(|s| s / bucket != leaf_bucket), "a node written into the bucket the listing leads into");
        },
        stalled,
    );
    gated.release();
    exec.block_on(async {}, stalled);
    exec.run_until_stalled();
    // Its first leaf as its pointer had it, the later ones as it reached
    // them: the 1 501 names less at most the 41 removed meanwhile.
    let got = listed.borrow_mut().take().expect("the listing ended").unwrap();
    assert!((1460..=1501).contains(&got), "{got} names listed");
    // The listing cached the old leaf at the bucket's sector; once it has
    // ended the bucket is taken again, and that node goes from the cache.
    exec.block_on(
        async {
            for n in names_made.iter().filter(|n| **n != in_leaf).skip(40).take(200) {
                m.unlink(m.root(), n, false, NOW).await.unwrap();
                if reclaim_all(&m).iter().any(|s| s / bucket == leaf_bucket) {
                    return;
                }
                m.commit().await.unwrap();
            }
            panic!("the leaf's bucket never taken again");
        },
        stalled,
    );
    drop(exec);
    block_on_ready(m.unmount()).unwrap();
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

/// A bucket freespace lists as empty that a backpointer leads into is
/// refused before anything is written there: the indexes are derived, and a
/// pointer may still lead into it. The mount stops by name.
#[test]
fn an_empty_bucket_a_backpointer_leads_into_is_refused() {
    let dev = formatted(64 << 20, None);
    let (first, sectors) = {
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        let mut b = None;
        block_on_ready(super::read::Source::walk(&v, Btree::Freespace, Bpos::MIN, |k, _| {
            b = Some((k.pos.offset & ((1 << 56) - 1)) - u64::from(k.size));
            Ok(false)
        }))
        .unwrap();
        (b.unwrap(), u64::from(v.superblock().member.bucket_sectors))
    };
    let at = Bpos::new(0, (first * sectors) << 16, 0);
    rewrite_leaf(&dev, Btree::Backpointers, at, 0, |keys| keys.push((Key::new(kind::BACKPOINTER, at), super::value::backpointer(1, 1, 3, 0, 64, Bpos::MAX).to_vec())));
    {
        let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
        block_on_ready(m.create(m.root(), b"f", New::File, OWNER, NOW)).unwrap();
        block_on_ready(m.commit()).unwrap();
        // A crash, so the next write to every node goes to a new one.
    }
    let m = block_on_ready(Mount::open(&dev, MEMORY, random())).unwrap();
    let why = "a bucket an index lists as empty that a backpointer leads into";
    let got = Executor::new().block_on(async { m.unmount().await }, || panic!("stalled"));
    assert_eq!(got, Err(Error::Corrupt(why)));
}

/// A need_discard bucket whose emptying sequence is past the volume's
/// journal is found: no entry the volume holds emptied it, so whatever
/// reuses it once that sequence is durable reuses it too early.
#[test]
fn a_bucket_emptied_past_the_journal_is_found() {
    let dev = fixture("links.img.gz");
    let (b, _) = need_discard(&block_on_ready(Volume::open(&dev)).unwrap())[0];
    let at = Bpos::new(0, b, 0);
    rewrite_leaf(&dev, Btree::Alloc, at, 0, |keys| {
        let (_, v) = keys.iter_mut().find(|(k, _)| k.pos == at).unwrap();
        v[48..56].copy_from_slice(&(1u64 << 40).to_le_bytes());
    });
    assert!(findings(&dev).contains(&Finding::Derived { btree: Btree::Alloc, pos: at, rule: "emptied past the journal" }));
}
