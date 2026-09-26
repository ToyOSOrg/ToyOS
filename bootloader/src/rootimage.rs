//! The boot disk as the loader reads it: its slot table, and a slot's ROOT
//! read whole into memory the kernel keeps, so the kernel mounts `/system`
//! from these bytes and needs no storage driver to boot.
//!
//! **Which disk** is the one firmware loaded this image from and no other: a
//! slot is a pair of partitions on one disk, and a slot table on another disk
//! is another machine's business. **Which ROOT** is the one the slot table
//! names by its unique GUID, `toyos_gpt::locate`d on that disk, and how many
//! bytes of it is what the slot's signed header says — never the partition's
//! length, and never a superblock's word: the header's SHA-256 is what those
//! bytes are held to (`crate::slot`).
//!
//! **The contract with the kernel**: [`RootImage`] is made only by
//! [`Disk::read_root`], its pages are `LoaderData`, which the kernel keeps out
//! of its allocator by the address it is handed, as it keeps the black box's
//! page, and nothing writes them between the read and the jump. A check over
//! the image between the read and [`RootImage::handoff`] is therefore a check
//! over exactly what the kernel mounts.

use alloc::alloc::Layout;
use alloc::string::String;
use core::num::NonZeroU64;

use toyos_gpt::{Guid, Partition, Sectors};
use toyos_rootimage::chunk;
use toyos_update::slots::{self, Table};
use uefi::prelude::*;
use uefi::proto::device_path::{DevicePath, DevicePathNode, DeviceSubType, DeviceType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::{BlockIO, BlockIoProtocol};
use uefi::table::boot::{AllocateType, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};

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

/// UEFI 2.11 §13.9's `EFI_BLOCK_IO_PROTOCOL_REVISION3`, the first whose media
/// carries `OptimalTransferLengthGranularity`.
const BLOCK_IO_REVISION3: u64 = 0x0002_001F;

/// The line a read ROOT is reported on, with where it went and what it cost.
pub const READ_AT: &str = "ROOT: read into memory at";

/// ROOT in memory: `len` bytes at the physical address `at`, identity-mapped
/// while boot services live.
pub struct RootImage {
    at: u64,
    len: u64,
    /// The partition it was read from, raw as in its GPT entry.
    partition: [u8; 16],
    /// The TSC cycles the read took.
    cycles: u64,
}

impl RootImage {
    /// Where the kernel is told the image is, which partition it came from, and
    /// the cycles reading it took.
    pub fn handoff(&self) -> (u64, u64, [u8; 16], u64) {
        (self.at, self.len, self.partition, self.cycles)
    }

    /// The image's bytes, for the hash its slot's header names.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `at` and `len` are the pages `Disk::read_root` allocated and
        // filled, identity-mapped while boot services live, written by nothing
        // after that read, and released only by `free`, which takes `self`.
        unsafe { core::slice::from_raw_parts(self.at as *const u8, self.len as usize) }
    }

    /// Give the pages back: a ROOT whose slot was refused is not the kernel's.
    pub fn free(self, bs: &BootServices) {
        // SAFETY: the pages are this image's own allocation, and `self` is
        // consumed, so nothing reads them after this.
        let freed = unsafe { bs.free_pages(self.at, (self.len as usize).div_ceil(BLOCK)) };
        if let Err(e) = freed {
            println!("ROOT: firmware would not take back the {} bytes at {:#x} ({e:?})", self.len, self.at);
        }
    }
}

/// The whole-disk block device carrying the partition firmware loaded this
/// image from: the one handle whose device path is the partition's without
/// its last node, the HARDDRIVE one.
pub fn boot_disk(handle: Handle, bs: &BootServices) -> Result<Handle, String> {
    let image = bs
        .open_protocol_exclusive::<LoadedImage>(handle)
        .map_err(|e| alloc::format!("this image's LoadedImage: {e:?}"))?;
    let device = image.device().ok_or("firmware names no device this image was loaded from")?;
    let path = try_get_protocol::<DevicePath>(bs, device)
        .map_err(|e| alloc::format!("the boot device's path: {e:?}"))?;
    let nodes: alloc::vec::Vec<&DevicePathNode> = path.node_iter().collect();
    let Some((last, disk_nodes)) = nodes.split_last() else {
        return Err("the boot device's path is empty".into());
    };
    if last.full_type() != (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE) {
        return Err("the boot device is not a partition, so there is no disk to find the slots on".into());
    }

    let handles = bs
        .find_handles::<BlockIO>()
        .map_err(|e| alloc::format!("firmware lists no block devices: {e:?}"))?;
    let disks: alloc::vec::Vec<Handle> = handles
        .into_iter()
        // Not the partition itself, whose path is held open above: a second
        // open by this agent is closed along with the first, and the second
        // close then fails.
        .filter(|&candidate| candidate != device)
        .filter(|&candidate| {
            let Ok(path) = try_get_protocol::<DevicePath>(bs, candidate) else { return false };
            path.node_iter().eq(disk_nodes.iter().copied())
        })
        .collect();
    match disks[..] {
        [disk] => Ok(disk),
        _ => Err(alloc::format!("{} block devices answer to the boot partition's disk path, wanted one", disks.len())),
    }
}

fn try_get_protocol<P: uefi::proto::ProtocolPointer + ?Sized>(
    bs: &BootServices,
    handle: Handle,
) -> uefi::Result<ScopedProtocol<'_, P>> {
    // SAFETY: `open_protocol`'s obligation is that the handle and protocol stay
    // installed until the `ScopedProtocol` drops. This loader is the one image
    // running, registers no callback that could uninstall either, and calls no
    // boot service that connects or disconnects a controller.
    //
    // Never exclusive: EXCLUSIVE stops every driver holding the protocol, and
    // on a disk that is the partition driver the ESP's filesystem sits on.
    unsafe {
        bs.open_protocol::<P>(
            OpenProtocolParams { handle, agent: bs.image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}

/// The boot disk, read through the firmware's block I/O.
pub struct Disk<'a> {
    io: ScopedProtocol<'a, BlockIO>,
    media_id: u32,
    lba_bytes: u32,
    lba_count: u64,
    /// One `BLOCK`, aligned for any `IoAlign` this accepts: the GPT parser's
    /// buffer carries no alignment of its own.
    scratch: &'static mut [u8],
}

impl<'a> Disk<'a> {
    pub fn open(bs: &'a BootServices, handle: Handle) -> Result<Self, String> {
        let io = try_get_protocol::<BlockIO>(bs, handle).map_err(|e| alloc::format!("the boot disk's block I/O: {e:?}"))?;
        let media = io.media();
        if !media.is_media_present() {
            return Err("the boot disk reports no media".into());
        }
        let lba_bytes = media.block_size();
        // UEFI 2.11 §13.9: `IoAlign` is 0 or 1 for none, else a power of two.
        let align = media.io_align().max(1) as usize;
        if !align.is_power_of_two() || align > BLOCK {
            return Err(alloc::format!("the boot disk wants buffers aligned to {align} bytes"));
        }
        if lba_bytes == 0 || !BLOCK.is_multiple_of(lba_bytes as usize) {
            return Err(alloc::format!("the boot disk's {lba_bytes}-byte block does not divide {BLOCK}"));
        }
        let (media_id, lba_count) = (media.media_id(), media.last_block() + 1);
        Ok(Disk { media_id, lba_bytes, lba_count, io, scratch: aligned(BLOCK) })
    }

    /// The partition `guid` names on this disk, checked against the table as
    /// `toyos_gpt::locate` checks every one.
    pub fn locate(&mut self, guid: [u8; 16]) -> Result<Partition, String> {
        toyos_gpt::locate(self, Guid(guid))
            .map(|located| located.partition)
            .map_err(|e| alloc::format!("partition {} on the boot disk: {e:?}", Guid(guid)))
    }

    /// The slot table on this disk's one TOYOS-SLOTS partition.
    pub fn slot_table(&mut self) -> Result<Table, String> {
        let blank = Partition { index: 0, type_guid: Guid::ZERO, unique_guid: Guid::ZERO, first_lba: 0, last_lba: 0 };
        let mut found = [blank; 2];
        let scan = toyos_gpt::locate_type(self, Guid::TOYOS_SLOTS, &mut found)
            .map_err(|e| alloc::format!("the boot disk's partition table: {e:?}"))?;
        if scan.matched != 1 {
            return Err(alloc::format!("the boot disk carries {} slot tables, and a machine has one", scan.matched));
        }
        let part = self.locate(found[0].unique_guid.0)?;
        let lbas = BLOCK as u64 / u64::from(self.lba_bytes);
        if part.lba_count() < slots::COPIES * lbas {
            return Err(alloc::format!("the slot table's partition is {} blocks, short of its two copies", part.lba_count()));
        }
        let mut copies = [[0u8; BLOCK]; 2];
        for (i, copy) in copies.iter_mut().enumerate() {
            let at = part.first_lba + i as u64 * lbas;
            self.io
                .read_blocks(self.media_id, at, self.scratch)
                .map_err(|e| alloc::format!("the slot table's copy {i} would not read: {:?}", e.status()))?;
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
    pub fn read_root(&mut self, bs: &BootServices, part: &Partition, len: u64) -> Result<RootImage, String> {
        let capacity = part.lba_count().saturating_mul(u64::from(self.lba_bytes));
        if len > capacity || !len.is_multiple_of(BLOCK as u64) {
            return Err(alloc::format!("{len} bytes of ROOT do not fit whole in its {capacity}-byte partition"));
        }
        let pages = (len / BLOCK as u64) as usize;
        // `LoaderData`, as the black box's page is, for the reason its module
        // header gives.
        let at = bs
            .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
            .map_err(|e| alloc::format!("firmware would not give {len} bytes for ROOT: {e:?}"))?;
        // SAFETY: the `pages` pages at `at` were just allocated to this loader,
        // are identity-mapped while boot services live, and nothing else holds them.
        let into = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, len as usize) };

        let granularity = self.granularity_lbas();
        // Chunks are whole `BLOCK`s from a page-aligned buffer, so each one
        // keeps the `IoAlign` `open` checked against `BLOCK`.
        let chunk = chunk::chunk_bytes(CHUNK_BOUND, BLOCK, self.lba_bytes, granularity.unwrap_or(0));
        let began = crate::tsc();
        let mut device = Firmware { io: &self.io, media_id: self.media_id };
        let read = chunk::read(&mut device, part.first_lba, self.lba_bytes, chunk, into);
        let image = RootImage { at, len, partition: part.unique_guid.0, cycles: crate::tsc().wrapping_sub(began) };
        if let Err(failed) = read {
            let why = alloc::format!(
                "the read of {} blocks at LBA {} failed: {:?}, after {} of {len} bytes read",
                failed.blocks,
                failed.lba,
                failed.error.status(),
                failed.read
            );
            image.free(bs);
            return Err(why);
        }
        println!(
            "{READ_AT} {at:#x}+{len:#x} from LBA {}+{}, {chunk} bytes a request (optimal granularity: {}), in {} TSC cycles",
            part.first_lba,
            len / u64::from(self.lba_bytes),
            match granularity {
                Some(lbas) => alloc::format!("{lbas} block(s)"),
                None => String::from("not reported"),
            },
            image.cycles
        );
        Ok(image)
    }

    /// `OptimalTransferLengthGranularity`, where the media is revision 3 or
    /// later and so carries the field, and reports a non-zero one.
    fn granularity_lbas(&self) -> Option<u32> {
        let io: &BlockIO = &self.io;
        // SAFETY: uefi 0.26 declares `BlockIO` `repr(transparent)` over
        // `BlockIoProtocol`, so the one is the other's layout; `revision` is
        // the field the crate does not expose.
        let revision = unsafe { &*core::ptr::from_ref(io).cast::<BlockIoProtocol>() }.revision;
        if revision < BLOCK_IO_REVISION3 {
            return None;
        }
        Some(self.io.media().optimal_transfer_length_granularity()).filter(|&lbas| lbas != 0)
    }
}

/// The boot disk as [`chunk::read`] asks for it.
struct Firmware<'a> {
    io: &'a BlockIO,
    media_id: u32,
}

impl chunk::Blocks for Firmware<'_> {
    type Error = uefi::Error;
    fn read(&mut self, lba: u64, into: &mut [u8]) -> uefi::Result {
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

    fn lba_count_granularity(&self) -> NonZeroU64 {
        NonZeroU64::MIN
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
