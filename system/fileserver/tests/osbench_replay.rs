//! osbench's `persist` and `fs.disk` file sequence and the shutdown sync,
//! against the DATA role on a model of the bench's partition, in the states
//! the benches' DATA went through: fresh, the native mkfs over the private
//! format's volume, and that volume after the private format's server, which
//! mounted it from its backup superblock, wrote it again. The role either refuses
//! the partition by name and writes nothing, or formats it whole; a volume it
//! mounts is judged by `fsck` and a fresh mount after the sequence.

use std::cell::Cell;

use bcachefs::engine::{fsck, Volume as Native};
use fileserver::cache::Disk;
use fileserver::data::{DataVolume, Probed};
use fileserver::ram::Ram;
use fileserver::volume::{Buf, OpenHow, Volume, Writes};
use toyos_blockdev::{block_on_ready, Block, BlockDev, Error, BLOCK};

const MIB: usize = 1 << 20;
const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
const CREATE_TRUNC: OpenHow = OpenHow { create: true, create_new: false, truncate: true };
const PLAIN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };
const ROOTS: [&str; 4] = ["apps", "config", "home", "state"];

/// A DATA partition of 1 GiB, sparse in memory: past the places that
/// matter, the native copies' and the private format's superblocks', the
/// benches' 170 and 600 GiB add only buckets.
const BLOCKS: u64 = 1 << 18;
/// The private format's bitmap on the T14: blocks 1 to 1360, the native
/// copies' places among them.
const BITMAP: u64 = 1360;
/// The sequential rows' file: 64 MiB on the bench, a 4 KiB call 512 times here.
const SEQ: usize = 2 * MIB;

fn clock() -> u64 {
    1_793_650_328_000_000_000
}

/// The partition, counting the writes it takes.
struct Model {
    ram: Ram,
    writes: Cell<u64>,
}

impl Model {
    fn new() -> Self {
        Self { ram: Ram::new(BLOCKS), writes: Cell::new(0) }
    }

    fn block(&self, lba: u64) -> Block {
        let mut b = [[0u8; BLOCK]];
        block_on_ready(self.ram.read(lba, &mut b)).unwrap();
        b[0]
    }

    fn put(&self, lba: u64, block: Block) {
        block_on_ready(self.ram.write(lba, &[block])).unwrap();
    }
}

impl BlockDev for Model {
    fn blocks(&self) -> u64 {
        self.ram.blocks()
    }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.ram.read(lba, out).await
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        self.writes.set(self.writes.get() + data.len() as u64);
        self.ram.write(lba, data).await
    }
    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

impl Disk for &Model {}

type Vol<'a> = DataVolume<&'a Model>;

fn probe(m: &Model) -> Probed<&Model> {
    DataVolume::probe(m, 16 * 1024, &ROOTS, clock)
}

/// The private format's superblock, as its server wrote both copies.
fn private_superblock() -> Block {
    let mut b = [0u8; BLOCK];
    b[..4].copy_from_slice(b"BCFS");
    b[4] = 2;
    b[12..20].copy_from_slice(&BLOCKS.to_le_bytes());
    b
}

/// The partition as the private format left it before any native format:
/// its superblock at both ends, a full bitmap, nodes and file data.
fn private_volume(m: &Model) {
    m.put(0, private_superblock());
    m.put(BLOCKS - 1, private_superblock());
    for b in 1..=BITMAP {
        m.put(b, [0xff; BLOCK]);
    }
    for b in BITMAP + 1..BITMAP + 4000 {
        let mut block = [(b % 251) as u8; BLOCK];
        if b % 3 == 0 {
            block[..4].copy_from_slice(b"BTND");
        }
        m.put(b, block);
    }
}

/// Block 0 designates the partition for a format, as `disks` writes it.
fn designate(m: &Model) {
    let mut stamp = [0u8; BLOCK];
    stamp.copy_from_slice(&bcachefs::designation(BLOCKS));
    m.put(0, stamp);
}

/// The private format's server mounting the volume the native mkfs left,
/// from its backup: both its superblocks written, its bitmap's first and
/// 257th blocks read, ored and written back over the native copies there
/// (`ored_from` bytes on, as far as its allocations reached), and its nodes
/// and data written into blocks the native volume lays out.
fn private_server_writes(m: &Model, ored_from: usize) {
    m.put(0, private_superblock());
    m.put(BLOCKS - 1, private_superblock());
    for b in [1, 257] {
        let mut block = m.block(b);
        for byte in &mut block[ored_from..] {
            *byte = 0xff;
        }
        m.put(b, block);
    }
    for b in (1361..40_000).step_by(7) {
        m.put(b, [0x2d; BLOCK]);
    }
}

fn put(v: &mut Vol, path: &str, data: &[u8]) {
    let n = v.open(path, CREATE_TRUNC).unwrap();
    v.write(n, 0, data).unwrap();
    v.close(n).unwrap();
}

fn remove_all(v: &mut Vol, dir: &str) {
    for e in v.list(dir).unwrap() {
        let p = format!("{dir}/{}", e.name);
        if v.lstat(&p).unwrap().kind == fileserver::volume::Kind::Dir {
            remove_all(v, &p);
        } else {
            v.unlink(&p).unwrap();
        }
    }
    v.rmdir(dir).unwrap();
}

fn page_at(i: usize, pages: usize) -> u64 {
    let x = (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 20;
    (x % pages as u64) * 4096
}

/// `tests/guest/osbench/src/storage.rs`'s durable rows, then `files.rs`'s
/// suite under the same directory, then the shutdown sync, each call as the
/// file server receives it. Every kind of call the rows make, a few times
/// each and on small files: what judges the volume is each call's effect,
/// which the rows' thousands of repetitions repeat.
fn osbench(v: &mut Vol) {
    let d = "home/osbench-1";
    v.mkdir(d).unwrap();
    let block = [0x6bu8; 4096];
    put(v, &format!("{d}/over"), &vec![1u8; MIB]);
    let f = v.open(&format!("{d}/over"), PLAIN).unwrap();
    for _ in 0..3 {
        v.write(f, 0, &block).unwrap();
        v.sync().unwrap();
    }
    for _ in 0..3 {
        v.sync().unwrap();
    }
    v.close(f).unwrap();
    let log = v.open(&format!("{d}/log"), CREATE).unwrap();
    for _ in 0..3 {
        let at = v.node_meta(log).unwrap().size;
        v.write(log, at, &block).unwrap();
        v.sync().unwrap();
    }
    v.close(log).unwrap();
    for _ in 0..3 {
        let n = v.open(&format!("{d}/fresh"), CREATE_TRUNC).unwrap();
        v.write(n, 0, &block).unwrap();
        v.sync().unwrap();
        v.close(n).unwrap();
        v.unlink(&format!("{d}/fresh")).unwrap();
    }
    let chunk = vec![0x2du8; MIB];
    for _ in 0..2 {
        let n = v.open(&format!("{d}/seq"), CREATE_TRUNC).unwrap();
        for i in 0..2 {
            v.write(n, (i * MIB) as u64, &chunk).unwrap();
        }
        v.sync().unwrap();
        v.close(n).unwrap();
    }
    v.unlink(&format!("{d}/seq")).unwrap();
    remove_all(v, d);

    v.mkdir(d).unwrap();
    let fill: Vec<u8> = (0..MIB).map(|i| (i % 251) as u8).collect();
    put(v, &format!("{d}/small"), &fill[..4096]);
    for _ in 0..3 {
        let n = v.open(&format!("{d}/fresh"), CREATE_TRUNC).unwrap();
        v.write(n, 0, &[7u8; 100]).unwrap();
        v.close(n).unwrap();
        v.unlink(&format!("{d}/fresh")).unwrap();
    }
    let big = format!("{d}/big");
    let n = v.open(&big, CREATE_TRUNC).unwrap();
    for i in 0..2 {
        v.write(n, (i * MIB) as u64, &fill).unwrap();
    }
    v.close(n).unwrap();
    let n = v.open(&big, PLAIN).unwrap();
    let mut buf = vec![0u8; 4096];
    for i in 0..8 {
        v.read(n, page_at(i, 512), &mut Buf(&mut buf)).unwrap();
        let at = page_at(i, 512) as usize % MIB;
        assert_eq!(buf, fill[at..at + 4096], "page {i} of the big file");
    }
    for i in 0..8 {
        v.write(n, page_at(i, 512), &buf).unwrap();
    }
    v.close(n).unwrap();
    v.unlink(&big).unwrap();
    let listed = format!("{d}/listed");
    v.mkdir(&listed).unwrap();
    for i in 0..10 {
        let n = v.open(&format!("{listed}/entry-{i:04}"), CREATE_TRUNC).unwrap();
        v.close(n).unwrap();
    }
    assert_eq!(v.list(&listed).unwrap().len(), 10);
    remove_all(v, &listed);
    let n = v.open(&format!("{d}/a"), CREATE_TRUNC).unwrap();
    v.close(n).unwrap();
    for i in 0..8 {
        let (from, to) = if i % 2 == 0 { ("a", "b") } else { ("b", "a") };
        v.rename(&format!("{d}/{from}"), &format!("{d}/{to}")).unwrap();
    }
    for _ in 0..3 {
        v.mkdir(&format!("{d}/sub")).unwrap();
        v.rmdir(&format!("{d}/sub")).unwrap();
    }
    for _ in 0..2 {
        let tree = format!("{d}/tree");
        v.mkdir(&tree).unwrap();
        for i in 0..10 {
            put(v, &format!("{tree}/f{i:03}"), &[3u8; 64]);
        }
        for i in 0..10 {
            v.unlink(&format!("{tree}/f{i:03}")).unwrap();
        }
        v.rmdir(&tree).unwrap();
    }
    let seq: Vec<u8> = (0..MIB).map(|i| (i % 253) as u8).collect();
    let path = format!("{d}/seq");
    for call in [4096usize, MIB] {
        for _ in 0..2 {
            let n = v.open(&path, CREATE_TRUNC).unwrap();
            for c in 0..SEQ / call {
                let at = c * call % MIB;
                v.write(n, (c * call) as u64, &seq[at..at + call]).unwrap();
            }
            v.close(n).unwrap();
        }
    }
    let n = v.open(&path, PLAIN).unwrap();
    let mut back = vec![0u8; SEQ];
    assert_eq!(v.read(n, 0, &mut Buf(&mut back)).unwrap(), back.len());
    assert!(back.chunks(MIB).all(|c| c == seq), "the sequential file reads back what was written");
    v.close(n).unwrap();
    v.unlink(&path).unwrap();
    put(v, "home/kept", &seq);
    remove_all(v, d);
    v.sync().unwrap();
}

/// The sequence on a mounted volume, then the volume judged: `fsck` finds
/// nothing, and a fresh mount finds the one file left as it was written.
fn replay_and_judge(m: &Model) {
    let Probed::Mounted(mut v) = probe(m) else { panic!("the volume did not mount") };
    osbench(&mut v);
    drop(v);
    let findings = block_on_ready(fsck(&block_on_ready(Native::open(m)).unwrap())).unwrap();
    assert_eq!(findings, [], "fsck after the sequence");
    let Probed::Mounted(mut again) = probe(m) else { panic!("the volume did not mount again") };
    let names: Vec<String> = again.list("home").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["kept"]);
    let n = again.open("home/kept", PLAIN).unwrap();
    let mut back = vec![0u8; MIB];
    again.read(n, 0, &mut Buf(&mut back)).unwrap();
    assert!(back.iter().enumerate().all(|(i, &b)| b == (i % 253) as u8), "the file kept reads back");
}

/// The probe of a partition it must refuse: by name, writing nothing.
fn refused(m: &Model, why: &str) {
    let before = m.writes.get();
    match probe(m) {
        Probed::Unmountable(said) => assert!(said.contains(why), "refused as {said:?}, not for {why:?}"),
        Probed::Foreign => assert_eq!(why, "foreign", "refused as foreign"),
        Probed::Mounted(_) => panic!("mounted a partition it should refuse ({why})"),
    }
    assert_eq!(m.writes.get(), before, "a refusal wrote to the partition");
}

/// The native mkfs leaves none of the private format's superblocks: block 0
/// is its own and the last block, where the private format kept its backup,
/// zeros.
fn no_private_superblock(m: &Model) {
    assert_ne!(&m.block(0)[..4], b"BCFS");
    assert_eq!(m.block(BLOCKS - 1), [0; BLOCK], "the private format's backup survived the format");
}

#[test]
fn a_fresh_designated_partition_is_formatted_and_survives_osbench() {
    let m = Model::new();
    refused(&m, "foreign");
    designate(&m);
    replay_and_judge(&m);
}

/// The benches' first step: the private format's volume, then the native
/// format. Undesignated, it is refused and untouched; designated, formatted
/// whole, with nothing of the private format left where a reader of that
/// format looks.
#[test]
fn the_private_format_s_volume_is_refused_until_designated_then_formatted_clean() {
    let m = Model::new();
    private_volume(&m);
    refused(&m, "foreign");
    designate(&m);
    let Probed::Mounted(v) = probe(&m) else { panic!("a designated partition did not format") };
    drop(v);
    no_private_superblock(&m);
    replay_and_judge(&m);
}

/// The benches' second step: a native volume the private format's server
/// then mounted from its backup and wrote — the T14's shape, its bitmap's
/// allocations ored over all but the first copy's header, and the AMD's,
/// where they reached nothing of the copy. Refused by name, untouched, until
/// designated again; then formatted clean.
#[test]
fn a_native_volume_the_private_format_wrote_over_is_refused_by_name() {
    for (bench, ored_from) in [("T14", 216), ("AMD", 2960)] {
        let m = Model::new();
        private_volume(&m);
        designate(&m);
        let Probed::Mounted(v) = probe(&m) else { panic!("{bench}: a designated partition did not format") };
        drop(v);
        // What the native mkfs that formatted the benches left: the private
        // format's backup at the last block.
        m.put(BLOCKS - 1, private_superblock());
        private_server_writes(&m, ored_from);
        refused(&m, "Overwritten { sector: 7 }");
        designate(&m);
        let Probed::Mounted(v) = probe(&m) else { panic!("{bench}: a designated partition did not format") };
        drop(v);
        no_private_superblock(&m);
        replay_and_judge(&m);
    }
}
