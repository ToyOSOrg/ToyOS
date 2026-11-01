//! The boot disk as the loader reads it: its slot table, and a slot's ROOT
//! read whole into memory the kernel keeps and hands the supervisor, whose
//! file server serves `/system` from these bytes, so no storage driver is
//! needed to boot.
//!
//! **Which disk** is the one firmware loaded this image from and no other: a
//! slot is a pair of partitions on one disk, and a slot table on another disk
//! is another machine's business. **Which ROOT** is the one the slot table
//! names by its unique GUID, `toyos_gpt::locate`d on that disk, and how many
//! bytes of it is what the slot's signed header says — never the partition's
//! length, and never a superblock's word: the header's SHA-256 is what those
//! bytes are held to (`crate::slot`).
//!
//! [`RootImage`] is made only by [`Disk::read_root`], and its pages are a
//! [`Payload`], whose contract with the kernel its module states.

use alloc::alloc::Layout;
use alloc::string::String;

use crate::payload::Payload;

use toyos_gpt::{Guid, Located, Sectors};
use toyos_rootimage::chunk;
use toyos_update::slots::{self, Table};
use toyos_devicepath::{HardDrive, Node};
use crate::efi::{BlockIo, BootServices, DevicePath, Handle, LoadedImage, Scoped, Status};

/// The unit ROOT's filesystem is written in, and the alignment every buffer
/// here is allocated at.
const BLOCK: usize = 4096;

/// The most one firmware read of ROOT asks for.
///
/// Block I/O reports no largest request it serves, so this is the loader's
/// choice and not a limit read off the media. 1 MiB keeps every request inside
/// one SCSI READ(10) at 512-byte blocks (SBC-3: a 16-bit count, 65535 blocks),
/// so a mass-storage driver that does not split has a legal command, and keeps
/// any bounce buffer a driver maps for DMA at that size rather than ROOT's; and
/// a read that fails is named to within 1 MiB.
const CHUNK_BOUND: usize = 1 << 20;

/// The line a read ROOT is reported on, with where it went and what it cost.
pub const READ_AT: &str = "ROOT: read into memory at";

/// ROOT in memory, and where it was read from.
pub struct RootImage {
    pub pages: Payload,
    /// The partition it was read from, raw as in its GPT entry.
    pub partition: [u8; 16],
    /// The counter ticks the read took.
    pub ticks: u64,
}

/// The whole-disk block device carrying the partition firmware loaded this
/// image from: the one handle whose device path is the partition's without
/// its last node, the HARDDRIVE one.
pub fn boot_disk(handle: Handle, bs: &BootServices) -> Result<Handle, String> {
    let image = bs
        .exclusive::<LoadedImage>(handle)
        .map_err(|e| alloc::format!("this image's LoadedImage: {e:?}"))?;
    let device = image.device().ok_or("firmware names no device this image was loaded from")?;
    let firmware = bs.get::<DevicePath>(device).map_err(|e| alloc::format!("the boot device's path: {e:?}"))?;
    let path = firmware.path().map_err(|why| alloc::format!("the boot device's path is not read: {why}"))?;
    let nodes: alloc::vec::Vec<Node<'_>> = path.nodes().collect();
    let Some((last, disk_nodes)) = nodes.split_last() else {
        return Err("the boot device's path is empty".into());
    };
    if last.kind() != HardDrive::TYPE {
        return Err("the boot device is not a partition, so there is no disk to find the slots on".into());
    }

    let handles = bs.handles::<BlockIo>().map_err(|e| alloc::format!("firmware lists no block devices: {e:?}"))?;
    let disks: alloc::vec::Vec<Handle> = handles
        .iter()
        .copied()
        // Not the partition itself, whose path is held open above: a second
        // open by this agent is closed along with the first, and the second
        // close then fails.
        .filter(|&candidate| candidate != device)
        .filter(|&candidate| {
            let Ok(firmware) = bs.get::<DevicePath>(candidate) else { return false };
            firmware.path().is_ok_and(|path| path.nodes().eq(disk_nodes.iter().copied()))
        })
        .collect();
    match disks[..] {
        [disk] => Ok(disk),
        _ => Err(alloc::format!("{} block devices answer to the boot partition's disk path, wanted one", disks.len())),
    }
}

/// The boot disk, read through the firmware's block I/O.
pub struct Disk<'a> {
    io: Scoped<'a, BlockIo>,
    media_id: u32,
    lba_bytes: u32,
    lba_count: u64,
    /// One `BLOCK`, aligned for any `IoAlign` this accepts: the GPT parser's
    /// buffer carries no alignment of its own.
    scratch: &'static mut [u8],
}

impl<'a> Disk<'a> {
    pub fn open(bs: &'a BootServices, handle: Handle) -> Result<Self, String> {
        let io = bs.get::<BlockIo>(handle).map_err(|e| alloc::format!("the boot disk's block I/O: {e:?}"))?;
        let media = io.media();
        if !media.is_media_present() {
            return Err("the boot disk reports no media".into());
        }
        let lba_bytes = media.block_size;
        // UEFI 2.11 §13.9: `IoAlign` is 0 or 1 for none, else a power of two.
        let align = media.io_align.max(1) as usize;
        if !align.is_power_of_two() || align > BLOCK {
            return Err(alloc::format!("the boot disk wants buffers aligned to {align} bytes"));
        }
        if lba_bytes == 0 || !BLOCK.is_multiple_of(lba_bytes as usize) {
            return Err(alloc::format!("the boot disk's {lba_bytes}-byte block does not divide {BLOCK}"));
        }
        let (media_id, lba_count) = (media.media_id, media.last_block + 1);
        Ok(Disk { media_id, lba_bytes, lba_count, io, scratch: aligned(BLOCK) })
    }

    /// The partition `guid` names on this disk, checked against the table as
    /// `toyos_gpt::locate` checks every one.
    pub fn locate(&mut self, guid: [u8; 16]) -> Result<Located, String> {
        toyos_gpt::locate(self, Guid(guid))
            .map_err(|e| alloc::format!("partition {} on the boot disk: {e:?}", Guid(guid)))
    }

    /// The slot table on this disk's one TOYOS-SLOTS partition.
    pub fn slot_table(&mut self) -> Result<Table, String> {
        let mut found = [None; 2];
        let scan = toyos_gpt::locate_type(self, Guid::TOYOS_SLOTS, &mut found)
            .map_err(|e| alloc::format!("the boot disk's partition table: {e:?}"))?;
        let listed = match (scan.matched, found[0]) {
            (1, Some(Ok(listed))) => listed,
            (1, Some(Err(unplaced))) => {
                return Err(alloc::format!("the boot disk's slot table entry is no partition on it: {unplaced:?}"));
            }
            (n, _) => return Err(alloc::format!("the boot disk carries {n} slot tables, and a machine has one")),
        };
        let part = self.locate(listed.unique_guid().0)?.partition();
        let lbas = BLOCK as u64 / u64::from(self.lba_bytes);
        if part.lba_count().get() < slots::COPIES * lbas {
            return Err(alloc::format!("the slot table's partition is {} blocks, short of its two copies", part.lba_count()));
        }
        let mut copies = [[0u8; BLOCK]; 2];
        for (i, copy) in copies.iter_mut().enumerate() {
            let at = part.first_lba() + i as u64 * lbas;
            self.io
                .read_blocks(self.media_id, at, self.scratch)
                .map_err(|e| alloc::format!("the slot table's copy {i} would not read: {e:?}"))?;
            copy.copy_from_slice(self.scratch);
        }
        slots::current([&copies[0], &copies[1]])
            .map(|(table, _)| table)
            .map_err(|why| alloc::format!("the slot table's partition holds {why}"))
    }

    /// The first `len` bytes of `part`, in chunks of at most [`CHUNK_BOUND`],
    /// into pages the kernel keeps.
    ///
    /// **Nothing bounds a chunk that never returns.** `ReadBlocks` takes no
    /// timeout and says nothing while it runs, so a firmware driver that stalls
    /// holds this loader until the firmware watchdog `main` armed at entry
    /// resets the machine, if the firmware honours it. What shows where it
    /// stopped is the slot's line before the read and the attempt count the
    /// next pass reads.
    pub fn read_root(&mut self, bs: &BootServices, part: &Located, len: u64) -> Result<RootImage, String> {
        let part = part.partition();
        let capacity = part.lba_count().get().saturating_mul(u64::from(self.lba_bytes));
        if len > capacity || !len.is_multiple_of(BLOCK as u64) {
            return Err(alloc::format!("{len} bytes of ROOT do not fit whole in its {capacity}-byte partition"));
        }
        let mut pages = Payload::allocate(bs, len).map_err(|why| alloc::format!("ROOT: {why}"))?;
        let at = pages.handoff().addr;

        let granularity = self.granularity_lbas();
        // Chunks are whole `BLOCK`s from a page-aligned buffer, so each one
        // keeps the `IoAlign` `open` checked against `BLOCK`.
        let chunk = chunk::chunk_bytes(CHUNK_BOUND, BLOCK, self.lba_bytes, granularity.unwrap_or(0));
        let began = crate::arch::counter();
        let mut device = Firmware { io: &self.io, media_id: self.media_id };
        let read = chunk::read(&mut device, part.first_lba(), self.lba_bytes, chunk, pages.bytes_mut());
        let image = RootImage { pages, partition: part.unique_guid().0, ticks: crate::arch::counter().wrapping_sub(began) };
        if let Err(failed) = read {
            let why = alloc::format!(
                "the read of {} blocks at LBA {} failed: {:?}, after {} of {len} bytes read",
                failed.blocks,
                failed.lba,
                failed.error,
                failed.read
            );
            image.pages.free(bs);
            return Err(why);
        }
        println!(
            "{READ_AT} {at:#x}+{len:#x} from LBA {}+{}, {chunk} bytes a request (optimal granularity: {}), in {} counter ticks",
            part.first_lba(),
            len / u64::from(self.lba_bytes),
            match granularity {
                Some(lbas) => alloc::format!("{lbas} block(s)"),
                None => String::from("not reported"),
            },
            image.ticks
        );
        Ok(image)
    }

    /// `OptimalTransferLengthGranularity`, where the media is revision 3 or
    /// later and so carries the field, and reports a non-zero one.
    fn granularity_lbas(&self) -> Option<u32> {
        self.io.optimal_transfer_length_granularity().filter(|&lbas| lbas != 0)
    }
}

/// The boot disk as [`chunk::read`] asks for it.
struct Firmware<'a, 'b> {
    io: &'a Scoped<'b, BlockIo>,
    media_id: u32,
}

impl chunk::Blocks for Firmware<'_, '_> {
    type Error = Status;
    fn read(&mut self, lba: u64, into: &mut [u8]) -> Result<(), Status> {
        self.io.read_blocks(self.media_id, lba, into)
    }
}

impl Sectors for Disk<'_> {
    fn lba_bytes(&self) -> u32 {
        self.lba_bytes
    }

    fn lba_count(&self) -> u64 {
        self.lba_count
    }

    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let n = self.lba_bytes as usize;
        if self.io.read_blocks(self.media_id, lba, &mut self.scratch[..n]).is_err() {
            return false;
        }
        buf.copy_from_slice(&self.scratch[..n]);
        true
    }
}

/// `len` bytes at `BLOCK` alignment, for the life of the loader.
fn aligned(len: usize) -> &'static mut [u8] {
    let layout = Layout::from_size_align(len, BLOCK).expect("a BLOCK-aligned layout");
    // SAFETY: `layout` has non-zero size (`len` is `BLOCK` at the one caller).
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    assert!(!ptr.is_null(), "a {len}-byte buffer for the boot disk");
    // SAFETY: `ptr` is a fresh allocation of `len` bytes, never freed.
    unsafe { core::slice::from_raw_parts_mut(ptr, len) }
}
