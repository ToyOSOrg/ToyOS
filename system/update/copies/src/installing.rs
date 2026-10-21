//! update's install over partitions in memory: a signed image written into
//! the idle slot and marked, and one another key signed writing nothing.

use toyos_disk::DiskError;
use toyos_update::slots::{Slot, Table};

use toyos_disk::{Disk, BLOCK};
use toyos_update::image::{Header, Sections, SIGNED_BYTES};
use toyos_update::slots::{self, Which};
use toyos_update::sig;

use crate::install::*;

/// A FAT32 volume built from fatgen103 by the checker's tests, and not by the
/// driver an install writes with.
#[path = "../../../../lib/fat32/check/tests/common/mod.rs"]
mod spec_volume;

/// A partition in memory.
struct Mem(Vec<u8>);

impl Disk for Mem {
    fn blocks(&self) -> u64 {
        (self.0.len() / BLOCK) as u64
    }
    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
        let at = first as usize * BLOCK;
        out.copy_from_slice(self.0.get(at..at + out.len()).ok_or(DiskError::Range)?);
        Ok(())
    }
    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
        let at = first as usize * BLOCK;
        self.0.get_mut(at..at + data.len()).ok_or(DiskError::Range)?.copy_from_slice(data);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), DiskError> {
        Ok(())
    }
}

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
    (Mem(table), Mem(boot), Mem(vec![0; 16 * BLOCK]))
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

fn install_on(table: &mut Mem, boot: &mut Mem, root: &mut Mem, image: &[u8]) -> Result<Installed, String> {
    install(
        &sig::public_of(&SEED),
        table,
        Partition { disk: boot, unique: SLOT_B.boot },
        Partition { disk: root, unique: SLOT_B.root },
        &mut &image[..],
        1_717_245_296,
        |_| {},
    )
}

/// The file at `path` on `boot`'s volume, read with the driver.
fn file_on(boot: &mut Mem, path: &str) -> Vec<u8> {
    let mut fs = toyos_fat32::Fat32::mount(crate::install::volume::Cached::new(boot)).expect("the volume mounts");
    let len = fs.metadata(path).unwrap_or_else(|e| panic!("{path}: {e:?}")).len;
    let mut file = fs.open(path).unwrap_or_else(|e| panic!("{path}: {e:?}"));
    let mut bytes = vec![0u8; len as usize];
    assert_eq!(fs.read(&mut file, 0, &mut bytes).unwrap_or_else(|e| panic!("{path}: {e:?}")), bytes.len());
    bytes
}

/// A signed image is written into the idle slot — ROOT onto its partition,
/// the kernel, the boot parameter and the signed header onto its volume,
/// which the checker that shares no code with the driver finds clean — and
/// the slot is marked at the image's version.
#[test]
fn a_signed_image_is_written_into_the_idle_slot_and_marked() {
    let (mut table, mut boot, mut root) = machine();
    let image = image(&SEED, 200);
    let installed = install_on(&mut table, &mut boot, &mut root, &image).unwrap();
    assert_eq!((installed.version, installed.slot, installed.root_bytes), (200, Which::B, 3 * BLOCK as u64));

    let (marked, _) = slots::read(|copies| table.read(0, copies.as_flattened_mut())).unwrap();
    assert_eq!(marked.marked, Which::B);
    assert_eq!(marked.slot(Which::B).map(|s| s.version), Some(200));
    assert_eq!(marked.slot(Which::A), Some(SLOT_A), "the running slot is untouched");
    assert!(root.0[..3 * BLOCK] == root_bytes(), "slot B's ROOT is not the image's");

    let complaints = toyos_fat32_check::check(&boot.0[..spec_volume::VOLUME_BYTES]);
    assert!(complaints.is_empty(), "{}", toyos_fat32_check::describe(&complaints));
    assert_eq!(file_on(&mut boot, slots::KERNEL_FILE), KERNEL);
    assert_eq!(file_on(&mut boot, slots::CMDLINE_FILE), CMDLINE);
    assert_eq!(file_on(&mut boot, slots::SUPERVISOR_FILE), SUPERVISOR);
    assert_eq!(file_on(&mut boot, slots::FILESERVER_FILE), FILESERVER);
    assert_eq!(file_on(&mut boot, slots::SIGNED_FILE), image[..SIGNED_BYTES]);
}

/// A signed image whose supervisor is not the bytes its header names is
/// refused by that section's name before its slot's volume or table is
/// written, so the machine boots what it booted.
#[test]
fn an_image_whose_supervisor_was_bent_is_refused_and_marks_nothing() {
    let (mut table, mut boot, mut root) = machine();
    let (table_before, boot_before) = (table.0.clone(), boot.0.clone());
    let mut bent = image(&SEED, 200);
    bent[SIGNED_BYTES + KERNEL.len() + CMDLINE.len()] ^= 1;
    let refused = install_on(&mut table, &mut boot, &mut root, &bent).unwrap_err();
    assert!(refused.contains("supervisor is not the bytes its signed header names"), "{refused}");
    assert!(table.0 == table_before && boot.0 == boot_before, "a refused image wrote the table or the volume");
}

/// An image another key signed is refused before anything is written: every
/// partition reads as it did, so the machine boots what it booted.
#[test]
fn an_image_another_key_signed_writes_nothing() {
    let (mut table, mut boot, mut root) = machine();
    let before = (table.0.clone(), boot.0.clone(), root.0.clone());
    let refused = install_on(&mut table, &mut boot, &mut root, &image(&STRANGER, 200));
    assert!(refused.is_err_and(|why| why.contains("signature")), "an image another key signed was installed");
    assert!((table.0, boot.0, root.0) == before, "a refused image wrote a partition");
}
