//! update's install over partitions in memory: a signed image written into
//! the idle slot and marked, and one another key signed writing nothing.

use toyos_blockdev::{block_on_ready, BlockDev, Bytes, BLOCK};
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
