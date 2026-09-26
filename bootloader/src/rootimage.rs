//! ROOT, read whole into memory the kernel keeps: the kernel mounts `/system`
//! from these bytes and needs no storage driver to boot.
//!
//! **Which ROOT** is the boot parameter's `root=<uuid>` against each
//! TOYOS-ROOT-typed partition's filesystem, on the disk firmware loaded this
//! image from and on no other: a slot is a pair of partitions on one disk, and
//! a ROOT on another disk is another machine's business. Each candidate is
//! `toyos_gpt::locate`d, and its superblock is the one `Superblock::read`
//! decides, primary or backup, which is the decision the kernel's mount makes
//! over the same bytes. `toyos_rootimage::pick` makes the choice.
//!
//! **The contract with the kernel**: [`RootImage`] is made only by [`read`],
//! its pages are `LoaderData`, which the kernel keeps out of its allocator by
//! the address it is handed, as it keeps the black box's page, and nothing
//! writes them between the read and the jump. A check over the image between
//! [`read`] and [`RootImage::handoff`] is therefore a check over exactly what
//! the kernel mounts.

use alloc::alloc::Layout;
use alloc::vec::Vec;
use core::cell::Cell;
use core::num::NonZeroU64;

use bcachefs::{BlockBuf, BlockNum, DeviceError, FsError, FsUuid, Superblock, TransferError};
use toyos_abi::boot::WITHHOLD_ROOT_PARAM;
use toyos_gpt::{Guid, Partition, Sectors};
use toyos_rootimage::chunk;
use toyos_rootimage::pick::{pick, Refused};
use uefi::prelude::*;
use uefi::proto::device_path::{DevicePath, DevicePathNode, DeviceSubType, DeviceType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::{BlockIO, BlockIoProtocol};
use uefi::table::boot::{AllocateType, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};

/// The unit ROOT's filesystem is written in, and the alignment every buffer
/// here is allocated at.
const BLOCK: usize = 4096;

/// How many TOYOS-ROOT partitions the boot disk may offer: the two slots of a
/// self-updating machine, with room to name a third in the refusal.
const MAX_CANDIDATES: usize = 4;

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
}

/// Say `why` on the console and in `loader.log`, then refuse the boot.
fn refuse(why: core::fmt::Arguments) -> ! {
    println!("ROOT: REFUSED, {why}");
    panic!("ROOT: {why}");
}

/// Read the ROOT `params` names into memory, or `None` when the parameter
/// tells this loader to withhold it.
pub fn read(handle: Handle, system_table: &SystemTable<Boot>, params: &str) -> Option<RootImage> {
    if toyos_abi::boot::actuators(params).any(|token| token == WITHHOLD_ROOT_PARAM) {
        println!("ROOT: withheld on {WITHHOLD_ROOT_PARAM}; the kernel is handed no image");
        return None;
    }
    let Some(named) = toyos_abi::boot::root_uuid(params).and_then(FsUuid::parse) else {
        refuse(format_args!("the boot parameter names no root filesystem this loader can parse"));
    };

    let bs = system_table.boot_services();
    let disk = boot_disk(handle, bs);
    let mut sectors = Disk::open(bs, disk);
    let blank = Partition {
        index: 0,
        type_guid: Guid::ZERO,
        unique_guid: Guid::ZERO,
        first_lba: 0,
        last_lba: 0,
    };
    let mut found = [blank; MAX_CANDIDATES];
    let scan = toyos_gpt::locate_type(&mut sectors, Guid::TOYOS_ROOT, &mut found)
        .unwrap_or_else(|e| refuse(format_args!("the boot disk's partition table: {e:?}")));

    let mut candidates: Vec<((Partition, u64), Option<FsUuid>)> = Vec::new();
    for candidate in &found[..scan.listed] {
        let part = toyos_gpt::locate(&mut sectors, candidate.unique_guid)
            .unwrap_or_else(|e| {
                refuse(format_args!(
                    "the boot disk's table names the TOYOS-ROOT candidate {} and then refuses it: {e:?}",
                    candidate.unique_guid
                ))
            })
            .partition;
        let superblock = sectors.superblock(&part);
        println!(
            "ROOT: candidate {} at LBA {}..={} holds {}",
            part.unique_guid,
            part.first_lba,
            part.last_lba,
            match &superblock {
                Ok(sb) => alloc::format!("{}", sb.uuid),
                Err(e) => alloc::format!("no superblock this loader can read ({e:?})"),
            }
        );
        let superblock = superblock.ok();
        let blocks = superblock.as_ref().map_or(0, |sb| sb.block_count);
        candidates.push(((part, blocks), superblock.map(|sb| sb.uuid)));
    }
    let (root, blocks) = pick(&named, &candidates, scan.matched).unwrap_or_else(|refused| match refused {
        Refused::Unlisted { matched, listed } => refuse(format_args!(
            "the boot disk carries {matched} TOYOS-ROOT partitions and this loader looks at {listed}"
        )),
        Refused::Matches { matches, candidates } => refuse(format_args!(
            "root={named} matches {matches} of the {candidates} TOYOS-ROOT partition(s) on the boot disk"
        )),
    });
    Some(sectors.read_filesystem(bs, &root, blocks))
}

/// The whole-disk block device carrying the partition firmware loaded this
/// image from: the one handle whose device path is the partition's without
/// its last node, the HARDDRIVE one.
fn boot_disk(handle: Handle, bs: &BootServices) -> Handle {
    let image = bs
        .open_protocol_exclusive::<LoadedImage>(handle)
        .unwrap_or_else(|e| refuse(format_args!("this image's LoadedImage: {e:?}")));
    let Some(device) = image.device() else {
        refuse(format_args!("firmware names no device this image was loaded from"));
    };
    let path = get_protocol::<DevicePath>(bs, device);
    let nodes: Vec<&DevicePathNode> = path.node_iter().collect();
    let Some((last, disk_nodes)) = nodes.split_last() else {
        refuse(format_args!("the boot device's path is empty"));
    };
    if last.full_type() != (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE) {
        refuse(format_args!("the boot device is not a partition, so there is no disk to find ROOT on"));
    }

    let handles = bs
        .find_handles::<BlockIO>()
        .unwrap_or_else(|e| refuse(format_args!("firmware lists no block devices: {e:?}")));
    let disks: Vec<Handle> = handles
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
        [disk] => disk,
        _ => refuse(format_args!(
            "{} block devices answer to the boot partition's disk path, wanted one",
            disks.len()
        )),
    }
}

/// Open `P` on `handle` without taking it from whoever holds it.
///
/// Never exclusive: EXCLUSIVE stops every driver holding the protocol, and on
/// a disk that is the partition driver the ESP's filesystem sits on.
fn get_protocol<P: uefi::proto::ProtocolPointer + ?Sized>(
    bs: &BootServices,
    handle: Handle,
) -> ScopedProtocol<'_, P> {
    try_get_protocol(bs, handle)
        .unwrap_or_else(|e| refuse(format_args!("the boot disk's protocol: {e:?}")))
}

fn try_get_protocol<P: uefi::proto::ProtocolPointer + ?Sized>(
    bs: &BootServices,
    handle: Handle,
) -> uefi::Result<ScopedProtocol<'_, P>> {
    // SAFETY: `open_protocol`'s obligation is that the handle and protocol stay
    // installed until the `ScopedProtocol` drops. This loader is the one image
    // running, registers no callback that could uninstall either, and calls no
    // boot service that connects or disconnects a controller.
    unsafe {
        bs.open_protocol::<P>(
            OpenProtocolParams { handle, agent: bs.image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}

/// The boot disk, read through the firmware's block I/O.
struct Disk<'a> {
    io: ScopedProtocol<'a, BlockIO>,
    media_id: u32,
    lba_bytes: u32,
    lba_count: u64,
    /// One `BLOCK`, aligned for any `IoAlign` this accepts: the GPT parser's
    /// buffer carries no alignment of its own.
    scratch: &'static mut [u8],
}

impl<'a> Disk<'a> {
    fn open(bs: &'a BootServices, handle: Handle) -> Self {
        let io = get_protocol::<BlockIO>(bs, handle);
        let media = io.media();
        if !media.is_media_present() {
            refuse(format_args!("the boot disk reports no media"));
        }
        let lba_bytes = media.block_size();
        // UEFI 2.11 §13.9: `IoAlign` is 0 or 1 for none, else a power of two.
        let align = media.io_align().max(1) as usize;
        if !align.is_power_of_two() || align > BLOCK {
            refuse(format_args!("the boot disk wants buffers aligned to {align} bytes"));
        }
        if lba_bytes == 0 || !BLOCK.is_multiple_of(lba_bytes as usize) {
            refuse(format_args!("the boot disk's {lba_bytes}-byte block does not divide {BLOCK}"));
        }
        let scratch = aligned(BLOCK);
        Disk {
            media_id: media.media_id(),
            lba_bytes,
            lba_count: media.last_block() + 1,
            io,
            scratch,
        }
    }

    /// The superblock `Superblock::read` decides for `part` — block 0, or the
    /// backup at the last whole block when block 0 is not one — which is the
    /// decision the kernel's mount makes over the image; `Err` for a candidate
    /// that carries none. A read the disk would not do refuses the boot,
    /// naming the firmware's status.
    fn superblock(&self, part: &Partition) -> Result<Superblock, FsError> {
        let lbas = BLOCK as u64 / u64::from(self.lba_bytes);
        let view = View {
            io: &self.io,
            media_id: self.media_id,
            first_lba: part.first_lba,
            lbas,
            blocks: part.lba_count() / lbas,
            failed: Cell::new(None),
        };
        match Superblock::read(&view) {
            Err(FsError::DeviceRead(block, _)) => refuse(format_args!(
                "the read of block {} of the TOYOS-ROOT candidate {} failed: {:?}",
                block.raw(),
                part.unique_guid,
                view.failed.get().expect("a refused read recorded the firmware's status")
            )),
            decided => decided,
        }
    }

    /// The filesystem on `part`, its `blocks` from the partition's start, in
    /// chunks of at most [`CHUNK_BOUND`], into pages the kernel keeps.
    ///
    /// **Nothing bounds a chunk that never returns.** `ReadBlocks` takes no
    /// timeout and says nothing while it runs, so a firmware driver that stalls
    /// holds this loader until the firmware watchdog `main` armed at entry
    /// resets the machine, if the firmware honours it. What shows where it
    /// stopped is the `ROOT: candidate` line before the read and the attempt
    /// count the next pass reads.
    fn read_filesystem(&mut self, bs: &BootServices, part: &Partition, blocks: u64) -> RootImage {
        let Some(len) = blocks.checked_mul(BLOCK as u64) else {
            refuse(format_args!("ROOT's filesystem states {blocks} blocks, which is no byte length"));
        };
        let lbas = len / u64::from(self.lba_bytes);
        // `LoaderData`, as the black box's page is, for the reason its module
        // header gives.
        let at = bs
            .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, blocks as usize)
            .unwrap_or_else(|e| refuse(format_args!("firmware would not give {len} bytes for ROOT: {e:?}")));
        // SAFETY: the `blocks` pages at `at` were just allocated to this loader,
        // are identity-mapped while boot services live, and nothing else holds them.
        let into = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, len as usize) };

        let granularity = self.granularity_lbas();
        // Chunks are whole `BLOCK`s from a page-aligned buffer, so each one
        // keeps the `IoAlign` `open` checked against `BLOCK`.
        let chunk = chunk::chunk_bytes(CHUNK_BOUND, BLOCK, self.lba_bytes, granularity.unwrap_or(0));
        let began = crate::tsc();
        let mut device = Firmware { io: &self.io, media_id: self.media_id };
        let read = chunk::read(&mut device, part.first_lba, self.lba_bytes, chunk, into);
        if let Err(failed) = read {
            refuse(format_args!(
                "the read of {} blocks at LBA {} failed: {:?}, after {} of {len} bytes read",
                failed.blocks,
                failed.lba,
                failed.error.status(),
                failed.read
            ));
        }
        let took = crate::tsc().wrapping_sub(began);
        println!(
            "{READ_AT} {at:#x}+{len:#x} from LBA {}+{lbas}, {chunk} bytes a request (optimal granularity: {}), in {took} TSC cycles",
            part.first_lba,
            match granularity {
                Some(lbas) => alloc::format!("{lbas} block(s)"),
                None => alloc::string::String::from("not reported"),
            }
        );
        RootImage { at, len, partition: part.unique_guid.0, cycles: took }
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

/// One candidate partition as `bcachefs` reads a device: `blocks` whole
/// `BLOCK`s of `lbas` logical blocks each from `first_lba`, keeping the status
/// of a read the firmware refused.
struct View<'a> {
    io: &'a BlockIO,
    media_id: u32,
    first_lba: u64,
    lbas: u64,
    blocks: u64,
    failed: Cell<Option<Status>>,
}

/// A read the firmware attempted and failed.
struct Attempted;

impl TransferError for Attempted {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

impl bcachefs::BlockIO for View<'_> {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        assert!(block.raw() < self.blocks, "the loader read {block} of a {}-block candidate", self.blocks);
        self.io.read_blocks(self.media_id, self.first_lba + block.raw() * self.lbas, &mut buf.0).map_err(|e| {
            self.failed.set(Some(e.status()));
            DeviceError::classify(&Attempted)
        })
    }

    fn write_block(&self, block: BlockNum, _buf: &BlockBuf) -> Result<(), DeviceError> {
        panic!("the loader wrote {block} of a ROOT candidate, and it writes no disk");
    }

    fn block_count(&self) -> u64 {
        self.blocks
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
