//! update's install over partitions in memory: a signed image written into
//! the idle slot and marked, and one another key signed writing nothing.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use toyos_blockdev::{block_on_ready, Block, BlockDev, Bytes, Error, BLOCK};
use toyos_update::slots::{Slot, Table};

use toyos_update::image::{Header, Sections, SIGNED_BYTES};
use toyos_update::slots::{self, Which};
use toyos_update::sig;

use crate::install::*;

/// A FAT32 volume built from fatgen103 by the checker's tests, and not by the
/// driver an install writes with.
#[path = "../../../../lib/fat32/check/tests/common/mod.rs"]
mod spec_volume;

/// A partition in memory.
type Mem = Bytes<Vec<u8>>;

/// The seed the image is signed with, and the one an image no machine trusts
/// is.
const SEED: [u8; 32] = [0x42; 32];
const STRANGER: [u8; 32] = [0x24; 32];

const SLOT_A: Slot = Slot { boot: [1; 16], root: [2; 16], version: 100 };
const SLOT_B: Slot = Slot { boot: [3; 16], root: [4; 16], version: 0 };

const KERNEL: &[u8] = b"an update's kernel, whose bytes are no ELF";
const CMDLINE: &[u8] = b"update-test";
const SUPERVISOR: &[u8] = b"an update's supervisor";
const FILESERVER: &[u8] = b"an update's file server";

/// A machine running slot A, its B empty: the table's partition, and B's FAT
/// volume and ROOT.
fn machine() -> (Mem, Mem, Mem) {
    let mut table = vec![0u8; 2 * BLOCK];
    table[..BLOCK].copy_from_slice(&Table { sequence: 1, marked: Which::A, slots: [Some(SLOT_A), Some(SLOT_B)] }.encode());
    let mut volume = spec_volume::Volume::new();
    volume.finish();
    let mut boot = volume.bytes;
    boot.resize(boot.len().next_multiple_of(BLOCK), 0);
    (Bytes::new(table), Bytes::new(boot), Bytes::new(vec![0; 16 * BLOCK]))
}

/// Three blocks of ROOT, each opening with its own index.
fn root_bytes() -> Vec<u8> {
    (0..3 * BLOCK).map(|i| if i % BLOCK == 0 { (i / BLOCK) as u8 } else { (i * 7) as u8 }).collect()
}

/// The image `ssh <machine> update` takes: the signed header, then every
/// section, ROOT last.
fn image(seed: &[u8; 32], version: u64) -> Vec<u8> {
    let root = root_bytes();
    let sections = Sections { kernel: KERNEL, cmdline: CMDLINE, supervisor: SUPERVISOR, fileserver: FILESERVER, root: &root };
    let header = Header::of(version, &sections).encode();
    [&header[..], &sig::sign(seed, &header), KERNEL, CMDLINE, SUPERVISOR, FILESERVER, &root].concat()
}

fn install_on(table: &Mem, boot: &Mem, root: &Mem, image: &[u8]) -> Result<Installed, String> {
    block_on_ready(install(
        &sig::public_of(&SEED),
        table,
        Partition { disk: boot, unique: SLOT_B.boot },
        Partition { disk: root, unique: SLOT_B.root },
        &mut &image[..],
        1_717_245_296,
        |_| {},
    ))
}

/// The file at `path` on `boot`'s volume, read with the driver.
fn file_on(boot: &Mem, path: &str) -> Vec<u8> {
    block_on_ready(async {
        let mut fs = toyos_fat32::Fat32::mount(boot).await.expect("the volume mounts");
        let len = fs.metadata(path).await.unwrap_or_else(|e| panic!("{path}: {e:?}")).len;
        let mut file = fs.open(path).await.unwrap_or_else(|e| panic!("{path}: {e:?}"));
        let mut bytes = vec![0u8; len as usize];
        assert_eq!(fs.read(&mut file, 0, &mut bytes).await.unwrap_or_else(|e| panic!("{path}: {e:?}")), bytes.len());
        bytes
    })
}

/// The table `table` holds now.
fn table_on(table: &Mem) -> Table {
    let mut copies = [[0; BLOCK]; 2];
    block_on_ready(table.read(0, &mut copies)).unwrap();
    slots::current([&copies[0], &copies[1]]).unwrap().0
}

/// A signed image is written into the idle slot — ROOT onto its partition,
/// the kernel, the boot parameter and the signed header onto its volume,
/// which the checker that shares no code with the driver finds clean — and
/// the slot is marked at the image's version.
#[test]
fn a_signed_image_is_written_into_the_idle_slot_and_marked() {
    let (table, boot, root) = machine();
    let image = image(&SEED, 200);
    let installed = install_on(&table, &boot, &root, &image).unwrap();
    assert_eq!((installed.version, installed.slot, installed.root_bytes), (200, Which::B, 3 * BLOCK as u64));

    let marked = table_on(&table);
    assert_eq!(marked.marked, Which::B);
    assert_eq!(marked.slot(Which::B).map(|s| s.version), Some(200));
    assert_eq!(marked.slot(Which::A), Some(SLOT_A), "the running slot is untouched");
    assert!(root.get()[..3 * BLOCK] == root_bytes(), "slot B's ROOT is not the image's");

    let complaints = toyos_fat32_check::check(&boot.get()[..spec_volume::VOLUME_BYTES]);
    assert!(complaints.is_empty(), "{}", toyos_fat32_check::describe(&complaints));
    assert_eq!(file_on(&boot, slots::KERNEL_FILE), KERNEL);
    assert_eq!(file_on(&boot, slots::CMDLINE_FILE), CMDLINE);
    assert_eq!(file_on(&boot, slots::SUPERVISOR_FILE), SUPERVISOR);
    assert_eq!(file_on(&boot, slots::FILESERVER_FILE), FILESERVER);
    assert_eq!(file_on(&boot, slots::SIGNED_FILE), image[..SIGNED_BYTES]);
}

/// A signed image whose supervisor is not the bytes its header names is
/// refused by that section's name before its slot's volume or table is
/// written, so the machine boots what it booted.
#[test]
fn an_image_whose_supervisor_was_bent_is_refused_and_marks_nothing() {
    let (table, boot, root) = machine();
    let (table_before, boot_before) = (table.get().clone(), boot.get().clone());
    let mut bent = image(&SEED, 200);
    bent[SIGNED_BYTES + KERNEL.len() + CMDLINE.len()] ^= 1;
    let refused = install_on(&table, &boot, &root, &bent).unwrap_err();
    assert!(refused.contains("supervisor is not the bytes its signed header names"), "{refused}");
    assert!(*table.get() == table_before && *boot.get() == boot_before, "a refused image wrote the table or the volume");
}

/// An image another key signed is refused before anything is written: every
/// partition reads as it did, so the machine boots what it booted.
#[test]
fn an_image_another_key_signed_writes_nothing() {
    let (table, boot, root) = machine();
    let before = (table.get().clone(), boot.get().clone(), root.get().clone());
    let refused = install_on(&table, &boot, &root, &image(&STRANGER, 200));
    assert!(refused.is_err_and(|why| why.contains("signature")), "an image another key signed was installed");
    let after = (table.into_inner(), boot.into_inner(), root.into_inner());
    assert!(after == before, "a refused image wrote a partition");
}

/// Above the held volume: the blocks written into it since its last flush,
/// which it holds.
struct Holding<'a> {
    volume: crate::install::volume::Cached<'a, Below>,
    held: Rc<RefCell<BTreeSet<u64>>>,
}

impl BlockDev for Holding<'_> {
    fn blocks(&self) -> u64 {
        self.volume.blocks()
    }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.volume.read(lba, out).await
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        self.volume.write(lba, data).await?;
        self.held.borrow_mut().extend(lba..lba + data.len() as u64);
        Ok(())
    }
    async fn flush(&self) -> Result<(), Error> {
        self.volume.flush().await?;
        self.held.borrow_mut().clear();
        Ok(())
    }
}

/// Below the held volume: the partition, counting each read of a block the
/// volume holds.
struct Below {
    disk: Mem,
    held: Rc<RefCell<BTreeSet<u64>>>,
    reads: Cell<u32>,
    of_held: Cell<u32>,
}

impl BlockDev for Below {
    fn blocks(&self) -> u64 {
        self.disk.blocks()
    }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.reads.set(self.reads.get() + 1);
        if self.held.borrow().range(lba..lba + out.len() as u64).next().is_some() {
            self.of_held.set(self.of_held.get() + 1);
        }
        self.disk.read(lba, out).await
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        self.disk.write(lba, data).await
    }
    async fn flush(&self) -> Result<(), Error> {
        self.disk.flush().await
    }
}

/// A file of many clusters written onto the held volume: every block its
/// writes touch is read from the partition at most until it is held, and
/// never again, so a FAT entry's read-modify-write is no round trip.
#[test]
fn a_held_block_is_never_read_from_the_partition() {
    let (_, boot, _) = machine();
    let held = Rc::new(RefCell::new(BTreeSet::new()));
    let below = Below { disk: boot, held: Rc::clone(&held), reads: Cell::new(0), of_held: Cell::new(0) };
    let time = toyos_fat32::FatTime::from_unix_secs(1_717_245_296);
    let bytes: Vec<u8> = (0..256 * 1024u32).map(|i| (i % 251) as u8).collect();
    block_on_ready(async {
        let volume = Holding { volume: crate::install::volume::Cached::new(&below), held: Rc::clone(&held) };
        let mut fs = toyos_fat32::Fat32::mount(volume).await.expect("the volume mounts");
        fs.create_dir_all("toyos", time).await.unwrap();
        let mut file = fs.create(slots::KERNEL_FILE, time).await.unwrap();
        fs.write(&mut file, 0, &bytes).await.unwrap();
        fs.flush_meta(&mut file, time).await.unwrap();
        assert!(held.borrow().len() > 64, "the write held {} blocks", held.borrow().len());
    });
    assert!(below.reads.get() > 0);
    assert_eq!(below.of_held.get(), 0, "{} of {} partition reads were of held blocks", below.of_held.get(), below.reads.get());
}

/// What reached one of the three partitions, in the order it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Write(&'static str),
    Flush(&'static str),
}

/// A partition in memory that logs each write and flush beside the others'.
struct Logged {
    name: &'static str,
    disk: Mem,
    log: Rc<RefCell<Vec<Op>>>,
}

impl BlockDev for Logged {
    fn blocks(&self) -> u64 {
        self.disk.blocks()
    }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.disk.read(lba, out).await
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        self.log.borrow_mut().push(Op::Write(self.name));
        self.disk.write(lba, data).await
    }
    async fn flush(&self) -> Result<(), Error> {
        self.log.borrow_mut().push(Op::Flush(self.name));
        self.disk.flush().await
    }
}

/// An install makes ROOT and the volume durable before the table moves the
/// mark, and the mark durable last: each partition's flush follows its last
/// write, the table's one write follows both, and its flush ends the install.
#[test]
fn the_mark_moves_only_once_the_slot_is_durable() {
    let (table, boot, root) = machine();
    let log = Rc::new(RefCell::new(Vec::new()));
    let logged = |name, disk| Logged { name, disk, log: Rc::clone(&log) };
    let (table, boot, root) = (logged("table", table), logged("boot", boot), logged("root", root));
    let image = image(&SEED, 200);
    block_on_ready(install(
        &sig::public_of(&SEED),
        &table,
        Partition { disk: &boot, unique: SLOT_B.boot },
        Partition { disk: &root, unique: SLOT_B.root },
        &mut &image[..],
        1_717_245_296,
        |_| {},
    ))
    .unwrap();
    let log = log.borrow();
    let last = |op| log.iter().rposition(|&o| o == op).unwrap_or_else(|| panic!("no {op:?} in {log:?}"));
    let first = |op| log.iter().position(|&o| o == op).unwrap_or_else(|| panic!("no {op:?} in {log:?}"));
    for name in ["root", "boot"] {
        assert!(last(Op::Write(name)) < last(Op::Flush(name)), "{name} was not flushed after its last write: {log:?}");
        assert!(last(Op::Flush(name)) < first(Op::Write("table")), "the table moved before {name} was durable: {log:?}");
    }
    assert_eq!(log.iter().filter(|&&o| o == Op::Write("table")).count(), 1, "{log:?}");
    assert_eq!(log.last(), Some(&Op::Flush("table")), "{log:?}");
}
