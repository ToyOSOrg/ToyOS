//! Interleavings the ordering rules exist for, constructed: reclaim's
//! halves driven by hand between transactions and commits, then a crash
//! judged at every state the model leaves.

use std::cell::Cell;
use std::vec::Vec;

use toyos_blockdev::model::{Model, Search};
use toyos_blockdev::{block_on_ready, Block, BlockDev, Bytes, ReadOnlyBytes, BLOCK};

use super::super::crypt::oracle;
use super::super::mount::{Order, Plan};
use super::mount::{random, MEMORY, NOW, OWNER};
use super::*;

const MIN_LIMITS: Limits = Limits { memory: 1 << 20, work: 1 << 20 };

fn open_rw<D: BlockDev>(dev: D, passphrase: Option<&[u8]>) -> Mount<D> {
    match passphrase {
        Some(p) => block_on_ready(Mount::unlock(dev, p, MIN_LIMITS, MEMORY, random())),
        None => block_on_ready(Mount::open(dev, MEMORY, random())),
    }
    .unwrap()
}

fn open_ro<D: BlockDev>(dev: D, passphrase: Option<&[u8]>) -> Result<Volume<D>> {
    match passphrase {
        Some(p) => block_on_ready(Volume::unlock(dev, p, MIN_LIMITS)),
        None => block_on_ready(Volume::open(dev)),
    }
}

fn create<D: BlockDev>(m: &Mount<D>, name: &[u8]) {
    block_on_ready(m.create(m.root(), name, New::File, OWNER, NOW)).unwrap();
}

/// Reclaim's first half until no dirty node is left to capture: every
/// dirty node written, none of their pointer updates journaled.
fn write_all<D: BlockDev>(m: &Mount<D>) -> Vec<Plan> {
    let mut plans = Vec::new();
    loop {
        let batch = block_on_ready(m.write_batch(Order::OldestPin)).unwrap();
        if batch.is_empty() {
            return plans;
        }
        plans.extend(batch);
    }
}

fn names<D: BlockDev>(v: &Volume<D>) -> Vec<Vec<u8>> {
    let (entries, _) = block_on_ready(v.readdir(v.root(), 0, 1000)).unwrap();
    entries.into_iter().map(|e| e.name).collect()
}

fn secret() -> Vec<u8> {
    hex_secret()
}

/// `a` commits; its nodes are written; `b` commits before their pointer
/// updates are journaled; the crash follows. Every crash state keeps both:
/// the written nodes pin `a`'s entry until their pointers are journaled, so
/// `b`'s entry keeps it in the window. Red when the pin is released at
/// node-write completion: `b`'s entry then moves the window past `a`, whose
/// appended bsets no durable pointer reaches.
#[test]
fn a_commit_between_a_node_write_and_its_pointer_update_keeps_every_acknowledged_key() {
    let model = Model::new(formatted(64 << 20, None).into_inner(), 512);
    let m = open_rw(&model, None);
    create(&m, b"a");
    block_on_ready(m.commit()).unwrap();
    let plans = write_all(&m);
    assert!(plans.iter().any(|p| p.appended(&m).is_some()), "a's leaves appended to");
    create(&m, b"b");
    block_on_ready(m.commit()).unwrap();
    let search = model
        .check(1 << 10, 0xea51, |image| {
            let v = open_ro(ReadOnlyBytes::new(image).unwrap(), None).map_err(|e| format!("{e:?}"))?;
            let mut seen = names(&v);
            seen.sort();
            if seen != [b"a".to_vec(), b"b".to_vec()] {
                return Err(format!("after the crash: {seen:?}"));
            }
            Ok(())
        })
        .unwrap_or_else(|v| panic!("{v}"));
    assert_eq!(search, Search::Every(1), "everything committed is flushed");
    drop(plans);
}

/// The pre-crash append (§3.5, A3.30, A10.21), on an encrypted volume: T1
/// and T2 share entry `s`; the nodes are written between them, as bsets of
/// sequence `s` holding T1's keys; `s` becomes durable but the pointer
/// updates in `s + 1` do not. After replay the next write to each such node
/// holds T1's and T2's keys under `s` again. It goes to a fresh node, so no
/// nonce is used over two plaintexts. Red when it is appended at the old
/// offset: the same offset, node sequence and journal sequence over other
/// contents.
#[test]
fn the_first_write_to_a_node_after_a_crash_goes_to_a_fresh_node_and_repeats_no_nonce() {
    oracle::reset();
    let secret = secret();
    let p = Some(&secret[..]);
    let dev = Bytes::new(formatted(64 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF })).into_inner());
    let m = open_rw(&dev, p);
    create(&m, b"t1");
    let plans = write_all(&m);
    assert!(plans.iter().any(|p| p.appended(&m).is_some()), "T1's leaves appended to");
    create(&m, b"t2");
    block_on_ready(m.commit()).unwrap();
    block_on_ready(m.point_batch(plans)).unwrap();
    // The crash: `s` durable, the pointer updates of `s + 1` never written.
    let crashed = Bytes::new(dev.get().clone());
    drop(m);
    let m = open_rw(&crashed, p);
    block_on_ready(m.unmount()).unwrap();
    let v = open_ro(&crashed, p).unwrap();
    let mut seen = names(&v);
    seen.sort();
    assert_eq!(seen, [b"t1".to_vec(), b"t2".to_vec()]);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
    assert!(oracle::nonces() > 20, "{} nonces recorded", oracle::nonces());
    assert_eq!(oracle::reused(), Vec::<[u32; 4]>::new(), "nonces used over two plaintexts");
}

/// The bsets on the medium of the node at `sector`: each record's journal
/// sequence, its node sequence that of the first record. Clear on an
/// encrypted volume too (A10.17).
fn bsets(image: &[u8], sector: u64, node_bytes: usize) -> Vec<u64> {
    let node = &image[sector as usize * SECTOR..][..node_bytes];
    let seq = le64(node, 136);
    if seq == 0 {
        return Vec::new();
    }
    let mut out = alloc::vec![le64(node, 144)];
    for at in (BLOCK..node_bytes).step_by(BLOCK) {
        if le64(node, at + 16) == seq {
            out.push(le64(node, at + 24));
        }
    }
    out
}

/// A transaction applied but never committed; its nodes are written, each
/// bset naming its entry, which never becomes durable; the crash; a mount
/// and a clean unmount. Every bset on the medium naming a sequence past the
/// newest durable flush entry is then blacklisted, so no reader takes it,
/// and none is reached anyway. Red with a skip of 0: the sequences the
/// crashed mount had open would be named again.
#[test]
fn a_bset_naming_an_unflushed_sequence_never_reappears_unblacklisted() {
    let dev = formatted(64 << 20, None);
    let m = open_rw(&dev, None);
    // The entry going read-write restates mkfs's empty journal with.
    let durable = 1;
    create(&m, b"lost");
    let plans = write_all(&m);
    let sectors: Vec<u64> = plans.iter().flat_map(|p| p.sectors(&m)).collect();
    let crashed = Bytes::new(dev.get().clone());
    drop(m);
    let m = open_rw(&crashed, None);
    create(&m, b"after");
    block_on_ready(m.unmount()).unwrap();
    let v = open_ro(&crashed, None).unwrap();
    assert_eq!(names(&v), [b"after".to_vec()]);
    let node_bytes = v.superblock().node_sectors() as usize * SECTOR;
    let image = crashed.get();
    let named: Vec<u64> = sectors.iter().flat_map(|&s| bsets(&image, s, node_bytes)).filter(|&s| s > durable).collect();
    assert!(!named.is_empty(), "the crashed mount's bsets name its open entry");
    for seq in named {
        assert!(v.superblock().blacklisted(seq), "a bset naming {seq}, past the durable {durable}, not blacklisted");
    }
}

/// An appended bset whose pointer is durable, torn after the fact: any
/// subset of its sectors back to what the medium held before. The read
/// refuses every torn state by its checksum or MAC, never trusting the
/// bytes. Red with the check skipped.
fn a_torn_bset_is_refused(encryption: Option<Encryption>) {
    let secret = encryption.as_ref().map(|e| e.passphrase.to_vec());
    let p = secret.as_deref();
    let dev = formatted(64 << 20, encryption);
    let before = dev.get().clone();
    let m = open_rw(&dev, p);
    for i in 0..20 {
        create(&m, format!("x{i}").as_bytes());
    }
    let plans = write_all(&m);
    let appended: Vec<(u64, usize, usize)> = plans.iter().filter_map(|p| p.appended(&m)).collect();
    block_on_ready(m.point_batch(plans)).unwrap();
    block_on_ready(m.commit()).unwrap();
    drop(m);
    let after = dev.get().clone();
    // The sectors a bset's write changed; a padding sector it left as it
    // was tears nothing. The bset changing most: the dirents leaf's.
    let changed = |&(sector, at, len): &(u64, usize, usize)| -> (usize, Vec<usize>) {
        let start = sector as usize * SECTOR + at;
        let units = (0..len / SECTOR).filter(|u| before[start + u * SECTOR..][..SECTOR] != after[start + u * SECTOR..][..SECTOR]).collect();
        (start, units)
    };
    let (start, units) = appended.iter().map(changed).max_by_key(|(_, u)| u.len()).expect("a bset appended");
    assert!(units.len() > 1, "a bset across one changed sector");
    let mut refused = 0;
    for kept in 0..(1u32 << units.len()) - 1 {
        let mut image = after.clone();
        for (i, u) in units.iter().enumerate() {
            if kept >> i & 1 == 0 {
                let r = start + u * SECTOR..start + (u + 1) * SECTOR;
                image[r.clone()].copy_from_slice(&before[r]);
            }
        }
        let read = open_ro(Bytes::new(image), p).and_then(|v| block_on_ready(v.names()).map(|_| ()));
        // A torn record's checksum fails, unless its header sector is the
        // old one, which names no record of this node.
        let refusals = [Err(Error::Checksum("btree node")), Err(Error::Corrupt("btree node shorter than its pointer's written sectors"))];
        assert!(refusals.contains(&read), "torn state {kept:#b} of {} sectors: {read:?}", units.len());
        refused += 1;
    }
    assert!(refused > 0);
}

#[test]
fn a_torn_bset_is_refused_by_its_checksum() {
    a_torn_bset_is_refused(None);
}

#[test]
fn a_torn_encrypted_bset_is_refused_by_its_mac() {
    let secret = secret();
    a_torn_bset_is_refused(Some(Encryption { passphrase: &secret, kdf: MIN_KDF }));
}

/// A device that answers the first write `fail` picks with an error, after
/// the bytes reached the medium: a write that failed may still have.
struct Failing<'a> {
    dev: &'a Image,
    fail: Box<dyn Fn(u64) -> bool + 'a>,
    armed: Cell<bool>,
    failed: Cell<bool>,
    /// What each write carried, by block.
    writes: std::cell::RefCell<Vec<(u64, Vec<u8>)>>,
}

impl BlockDev for Failing<'_> {
    fn blocks(&self) -> u64 {
        self.dev.blocks()
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.read(lba, out).await
    }

    async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.write(lba, data).await?;
        self.writes.borrow_mut().push((lba, data.as_flattened().to_vec()));
        if self.armed.get() && !self.failed.get() && (self.fail)(lba) {
            self.failed.set(true);
            return Err(toyos_blockdev::Error::Device);
        }
        Ok(())
    }

    async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.flush().await
    }
}

/// A journal or node write that fails once, on an encrypted volume: it is
/// reissued byte for byte, and the mount carries on; no nonce is used over
/// two plaintexts. Red when the retry carries other contents.
fn a_failed_write_is_reissued_byte_for_byte(journal: bool) {
    oracle::reset();
    let secret = secret();
    let p = Some(&secret[..]);
    let dev = formatted(64 << 20, Some(Encryption { passphrase: &secret, kdf: MIN_KDF }));
    // The journal's blocks, and the nodes' beyond them short of the end
    // superblock copy.
    let (ring, nodes) = {
        let sb = block_on_ready(super::super::read::read_superblock(&dev)).unwrap();
        let bucket = u64::from(sb.member.bucket_sectors) / 8;
        let (first, count) = sb.journal[0];
        (first * bucket..(first + count) * bucket, (first + count) * bucket..sb.header.layout.offsets[2] / 8)
    };
    let failing = Failing {
        dev: &dev,
        fail: Box::new(move |lba| if journal { ring.contains(&lba) } else { nodes.contains(&lba) }),
        armed: Cell::new(false),
        failed: Cell::new(false),
        writes: std::cell::RefCell::default(),
    };
    let m = open_rw(&failing, p);
    let armed_at = failing.writes.borrow().len();
    failing.armed.set(true);
    for i in 0..30 {
        create(&m, format!("f{i}").as_bytes());
        if i % 3 == 2 {
            block_on_ready(m.commit()).unwrap();
        }
    }
    block_on_ready(m.unmount()).unwrap();
    assert!(failing.failed.get(), "no write failed");
    assert_eq!(oracle::reused(), Vec::<[u32; 4]>::new(), "nonces used over two plaintexts");
    let writes = failing.writes.borrow();
    let first = armed_at + writes[armed_at..].iter().position(|(lba, _)| (failing.fail)(*lba)).unwrap();
    let again = writes[first + 1..].iter().find(|(lba, _)| *lba == writes[first].0).expect("the failed write reissued");
    assert_eq!(again.1, writes[first].1, "the reissue carried other bytes");
    let v = open_ro(&dev, p).unwrap();
    assert_eq!(names(&v).len(), 30);
    assert_eq!(block_on_ready(fsck(&v)).unwrap(), []);
}

#[test]
fn a_failed_journal_write_is_reissued_byte_for_byte() {
    a_failed_write_is_reissued_byte_for_byte(true);
}

#[test]
fn a_failed_node_write_is_reissued_byte_for_byte() {
    a_failed_write_is_reissued_byte_for_byte(false);
}

/// A device whose writes each yield once, as a real one's do, and fail past
/// every retry once `broken` is set: a node write in flight while another
/// transaction reaches the node.
struct Broken<'a> {
    dev: &'a Image,
    broken: Cell<bool>,
}

impl BlockDev for Broken<'_> {
    fn blocks(&self) -> u64 {
        self.dev.blocks()
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.read(lba, out).await
    }

    async fn write(&self, lba: u64, data: &[Block]) -> core::result::Result<(), toyos_blockdev::Error> {
        let mut yielded = false;
        core::future::poll_fn(|cx| {
            if yielded {
                return core::task::Poll::Ready(());
            }
            yielded = true;
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        })
        .await;
        if self.broken.get() {
            return Err(toyos_blockdev::Error::Device);
        }
        self.dev.write(lba, data).await
    }

    async fn flush(&self) -> core::result::Result<(), toyos_blockdev::Error> {
        self.dev.flush().await
    }
}

/// A node write that fails past its retries while a transaction waits on
/// that node: the mount stops read-only, and both the reclaim and the
/// waiting transaction answer `ReadOnly` or the write's error, never a
/// stall; unmount refuses rather than spin.
#[test]
fn a_node_write_failing_past_its_retries_stops_the_mount_and_its_waiters() {
    use core::future::Future;
    use core::pin::Pin;
    use toyos_blockdev::executor::Executor;
    let dev = formatted(64 << 20, None);
    let broken = Broken { dev: &dev, broken: Cell::new(false) };
    let m = Executor::new().block_on(Mount::open(&broken, MEMORY, random()), || panic!("mount stalled")).unwrap();
    Executor::new().block_on(async { m.create(m.root(), b"a", New::File, OWNER, NOW).await.unwrap() }, || panic!("create stalled"));
    broken.broken.set(true);
    let reclaim = core::cell::RefCell::new(None);
    let late = core::cell::RefCell::new(None);
    let mut work: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = vec![
        Box::pin(async { *reclaim.borrow_mut() = Some(m.write_batch(Order::OldestPin).await.map(|p| p.len())) }),
        Box::pin(async { *late.borrow_mut() = Some(m.create(m.root(), b"b", New::File, OWNER, NOW).await.map(|i| i.ino)) }),
    ];
    Executor::new().block_on(
        core::future::poll_fn(|cx| {
            work.retain_mut(|f| f.as_mut().poll(cx).is_pending());
            if work.is_empty() { core::task::Poll::Ready(()) } else { core::task::Poll::Pending }
        }),
        || panic!("a writer stalled behind the failed write; {}", m.dump()),
    );
    drop(work);
    assert_eq!(reclaim.into_inner(), Some(Err(Error::Device(toyos_blockdev::Error::Device))));
    assert_eq!(late.into_inner(), Some(Err(Error::ReadOnly("btree node write failed"))));
    let unmounted = Executor::new().block_on(m.unmount(), || panic!("unmount stalled"));
    assert_eq!(unmounted, Err(Error::ReadOnly("btree node write failed")));
}

/// A plan that fails on the second node of its batch: after a crash every
/// node's first write is fresh, and on this volume each node takes a bucket
/// of its own, the second of which freespace lists but whose alloc key holds
/// data. The mount stops read-only by the failure's name, and the next
/// transaction and the unmount answer that rather than wait.
#[test]
fn a_reclaim_plan_failing_midway_stops_the_mount() {
    use super::super::bucket::Bucket;
    use toyos_blockdev::executor::Executor;
    let dev = formatted(64 << 20, None);
    let second_free = {
        let v = block_on_ready(Volume::open(&dev)).unwrap();
        assert_eq!(v.superblock().node_sectors(), u32::from(v.superblock().member.bucket_sectors), "a node to a bucket");
        let mut b = None;
        block_on_ready(super::super::read::Source::walk(&v, Btree::Freespace, Bpos::MIN, |k, _| {
            assert!(k.size >= 2);
            b = Some((k.pos.offset & ((1 << 56) - 1)) - u64::from(k.size) + 1);
            Ok(false)
        }))
        .unwrap();
        b.unwrap()
    };
    let at = Bpos::new(0, second_free, 0);
    let held = Bucket { data_type: super::super::value::data_type::USER, dirty: 8, ..Bucket::HOLE }.encode().to_vec();
    rewrite_leaf(&dev, Btree::Alloc, at, 0, |keys| keys.push((Key::new(kind::ALLOC_V4, at), held)));
    {
        let m = open_rw(&dev, None);
        create(&m, b"a");
        block_on_ready(m.commit()).unwrap();
        // A crash: the mount dropped, never unmounted.
    }
    let m = open_rw(&dev, None);
    let stalled = || panic!("stalled; {}", m.dump());
    create(&m, b"b");
    let first = Executor::new().block_on(m.write_batch(Order::OldestPin), stalled);
    assert_eq!(first.err(), Some(Error::Corrupt("a bucket an index lists as empty whose alloc key holds otherwise")));
    let late = Executor::new().block_on(async { m.create(m.root(), b"late", New::File, OWNER, NOW).await.map(|i| i.ino) }, stalled);
    assert_eq!(late, Err(Error::ReadOnly("a bucket an index lists as empty whose alloc key holds otherwise")));
    assert_eq!(Executor::new().block_on(m.unmount(), || panic!("unmount stalled")), Err(Error::ReadOnly("a bucket an index lists as empty whose alloc key holds otherwise")));
}
