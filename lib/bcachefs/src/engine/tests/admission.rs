//! Admission before the slot (§3.3): writers that wait on a full journal or
//! a full cache complete while reclaim runs beside them, and concurrent
//! writers keep the dirty-node limit. A deadlock is a stall of every task,
//! which the executor turns into a panic.

use core::future::Future;
use core::pin::Pin;
use core::task::Poll;
use std::boxed::Box;
use std::vec::Vec;

use toyos_blockdev::executor::Executor;
use toyos_blockdev::{block_on_ready, Block, BlockDev};

use super::mount::{random, NOW, OWNER};
use super::*;

/// A device whose every read yields once before it is answered, as a real
/// one's does: a writer holding the slot is pending on its read while the
/// others reach admission.
struct Yielding<'a>(&'a Image);

impl BlockDev for Yielding<'_> {
    fn blocks(&self) -> u64 {
        self.0.blocks()
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        let mut yielded = false;
        core::future::poll_fn(|cx| {
            if yielded {
                return Poll::Ready(());
            }
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        })
        .await;
        self.0.read(lba, out).await
    }

    async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.0.write(lba, data).await
    }

    async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
        self.0.flush().await
    }
}

/// What a run of writers left: the dirty-node limit, the most dirty and
/// unpointed nodes held at once, and the admissions that waited for journal
/// space and for cache room.
struct Pressure {
    limit: usize,
    room: usize,
    peak: usize,
    waits: [usize; 2],
}

/// `writers` tasks on `dev` mounted with a cache of `cache_nodes`, writer
/// `w` creating `per` files in `dirs[w % dirs.len()]`; then a clean unmount,
/// after which the volume checks clean.
fn writers(dev: &Image, cache_nodes: usize, dirs: &[u64], writers: usize, per: usize) -> Pressure {
    let device = Yielding(dev);
    let mount = Executor::new().block_on(Mount::open(&device, MountOptions { cache_nodes }, random()), || panic!("mount stalled")).unwrap();
    let exec = Executor::new();
    let mut work: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = (0..writers)
        .map(|w| {
            let (mount, dir) = (&mount, dirs[w % dirs.len()]);
            Box::pin(async move {
                for i in 0..per {
                    mount.create(dir, format!("w{w}-{i}").as_bytes(), New::File, OWNER, NOW).await.unwrap();
                }
            }) as Pin<Box<dyn Future<Output = ()>>>
        })
        .collect();
    exec.block_on(
        core::future::poll_fn(|cx| {
            work.retain_mut(|f| f.as_mut().poll(cx).is_pending());
            if work.is_empty() { Poll::Ready(()) } else { Poll::Pending }
        }),
        || panic!("every writer stalled; {}", mount.dump()),
    );
    let (limit, room, peak, waits) = mount.pressure();
    drop(work);
    drop(exec);
    Executor::new().block_on(mount.unmount(), || panic!("unmount stalled")).unwrap();
    let v = block_on_ready(Volume::open(dev)).unwrap();
    assert!(block_on_ready(v.names()).unwrap().len() >= writers * per);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    Pressure { limit, room, peak, waits }
}

/// The least cache a mount takes: room for one transaction's footprint
/// beside reclaim's, so each writer waits for reclaim to clean what the
/// last one dirtied.
const SMALL_CACHE: usize = 54;

/// Creates past what the journal holds, with no commit: entries fill and are
/// written, the ring fills with entries the dirty nodes pin, and a writer
/// waits for journal space until reclaim writes the nodes. Red when a
/// transaction takes journal space after taking the slot, or when reclaim
/// draws on ordinary admission: either is a stall.
#[test]
fn a_writer_waiting_on_a_full_journal_completes_while_reclaim_runs() {
    let dev = formatted(64 << 20, None);
    let p = writers(&dev, 256, &[mkfs::ROOT_INODE], 1, 600);
    assert!(p.waits[0] > 0, "no writer waited for the journal: {:?}", p.waits);
}

/// A writer whose every transaction fills the dirty-node limit waits for
/// cache room until reclaim cleans the last one's nodes. Red when cache room
/// is taken after the slot, or when reclaim draws on ordinary admission.
#[test]
fn a_writer_filling_the_cache_with_dirty_nodes_completes_while_reclaim_runs() {
    let dev = formatted(64 << 20, None);
    let p = writers(&dev, SMALL_CACHE, &[mkfs::ROOT_INODE], 1, 60);
    assert!(p.waits[1] > 0, "no writer waited for cache room: {:?}", p.waits);
}

/// Thirty-two writers at once, each in a directory whose names live in a
/// leaf of their own: admission counts what it has granted and not
/// yet applied against the limit, so their dirty nodes and reclaim's
/// together stay within the limit and reclaim's room, which the mount
/// asserts at every apply. Red when admission ignores outstanding grants:
/// thirty-two footprints at once pass it.
#[test]
fn concurrent_writers_keep_the_dirty_node_limit() {
    let mut tree = Tree::new();
    let meta = Meta { perm: 0o755, uid: 0, gid: 0 };
    let mut dirs = Vec::new();
    for d in 0..128 {
        let dir = tree.dir(Tree::ROOT, format!("d{d}").as_bytes(), meta).unwrap();
        for f in 0..100 {
            tree.file(dir, format!("f{f}").as_bytes(), b"", meta).unwrap();
        }
        dirs.push(d);
    }
    // 32 KiB nodes: every fourth directory's names in a leaf of their own.
    let dev = populated(64 << 20, &tree, 7);
    let v = block_on_ready(Volume::open(&dev)).unwrap();
    let inos: Vec<u64> = dirs.iter().map(|d| path(&v, &format!("d{d}"))).collect();
    let spread: Vec<u64> = (0..32).map(|w| inos[w * 4]).collect();
    let names: std::collections::BTreeSet<u64> = spread.iter().map(|&d| leaf_of(&v, Btree::Dirents, Bpos::new(d, 0, u32::MAX)).0.ptr.sector).collect();
    assert_eq!(names.len(), 32, "each writer's names in a leaf of their own");
    drop(v);
    let p = writers(&dev, SMALL_CACHE, &spread, 32, 2);
    assert!(p.peak <= p.limit + p.room, "{} dirty and unpointed nodes past the limit of {} and reclaim's {}", p.peak, p.limit, p.room);
    assert!(p.waits[1] > 0, "no writer waited for cache room: {:?}", p.waits);
}
