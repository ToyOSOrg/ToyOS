//! ROOT, read whole into memory the kernel keeps: the kernel mounts `/system`
//! from these bytes and needs no storage driver to boot.
//!
//! **Which ROOT** is the boot parameter's `root=<uuid>` against each
//! TOYOS-ROOT-typed partition's superblock, on the disk firmware loaded this
//! image from and on no other: a slot is a pair of partitions on one disk, and
//! a ROOT on another disk is another machine's business. None, or more than
//! one, refuses the boot by name.
//!
//! **The contract with the kernel**: [`RootImage`] is made only by [`read`],
//! its pages are [`ROOT_IMAGE_MEMORY_TYPE`] so the kernel's allocator never
//! takes them, and nothing writes them between the read and the jump. A check
//! over the image between [`read`] and [`RootImage::handoff`] is therefore a
//! check over exactly what the kernel mounts.

use alloc::alloc::Layout;
use alloc::vec::Vec;
use core::num::NonZeroU64;

use bcachefs::{BlockBuf, FsUuid, Superblock};
use toyos_abi::boot::{ROOT_IMAGE_MEMORY_TYPE, WITHHOLD_ROOT_PARAM};
use toyos_gpt::{Guid, Partition, Sectors};
use uefi::prelude::*;
use uefi::proto::device_path::{DevicePath, DevicePathNode, DeviceSubType, DeviceType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::table::boot::{AllocateType, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};

/// The unit ROOT's filesystem is written in, and the alignment every buffer
/// here is allocated at.
const BLOCK: usize = 4096;

/// How many TOYOS-ROOT partitions the boot disk may offer: the two slots of a
/// self-updating machine, with room to name a third in the refusal.
const MAX_CANDIDATES: usize = 4;

/// The line a read ROOT is reported on, with where it went and what it cost.
pub const READ_AT: &str = "ROOT: read into memory at";

/// ROOT in memory: `len` bytes at the physical address `at`, identity-mapped
/// while boot services live.
pub struct RootImage {
    at: u64,
    len: u64,
}

impl RootImage {
    /// Where the kernel is told the image is.
    pub fn handoff(&self) -> (u64, u64) {
        (self.at, self.len)
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

    let mut matched: Vec<Partition> = Vec::new();
    for candidate in &found[..scan.listed] {
        let uuid = sectors.superblock_uuid(candidate);
        println!(
            "ROOT: candidate {} at LBA {}..={} holds {}",
            candidate.unique_guid,
            candidate.first_lba,
            candidate.last_lba,
            match uuid {
                Some(uuid) => alloc::format!("{uuid}"),
                None => alloc::string::String::from("no superblock this loader can read"),
            }
        );
        if uuid == Some(named) {
            matched.push(*candidate);
        }
    }
    let [root] = matched[..] else {
        refuse(format_args!(
            "root={named} matches {} of the {} TOYOS-ROOT partition(s) on the boot disk",
            matched.len(),
            scan.matched
        ));
    };
    Some(sectors.read_partition(bs, &root))
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
        if lba_bytes == 0 || BLOCK % lba_bytes as usize != 0 {
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

    /// What the superblock at the start of `part` names, or `None` for a
    /// partition that carries none this loader parses.
    fn superblock_uuid(&mut self, part: &Partition) -> Option<FsUuid> {
        let lbas = BLOCK as u64 / u64::from(self.lba_bytes);
        if part.last_lba.checked_sub(part.first_lba)? + 1 < lbas {
            return None;
        }
        self.io.read_blocks(self.media_id, part.first_lba, self.scratch).ok()?;
        let block = BlockBuf(<[u8; BLOCK]>::try_from(&self.scratch[..]).ok()?);
        Superblock::parse(&block).ok().map(|sb| sb.uuid)
    }

    /// The whole of `part`, in one read, into pages the kernel keeps.
    fn read_partition(&mut self, bs: &BootServices, part: &Partition) -> RootImage {
        let blocks = part.last_lba - part.first_lba + 1;
        let Some(len) = blocks.checked_mul(u64::from(self.lba_bytes)).filter(|len| len % BLOCK as u64 == 0)
        else {
            refuse(format_args!("ROOT is {blocks} blocks of {} bytes, not whole {BLOCK}-byte blocks", self.lba_bytes));
        };
        let pages = (len / BLOCK as u64) as usize;
        let at = bs
            .allocate_pages(AllocateType::AnyPages, MemoryType::custom(ROOT_IMAGE_MEMORY_TYPE), pages)
            .unwrap_or_else(|e| refuse(format_args!("firmware would not give {len} bytes for ROOT: {e:?}")));
        // SAFETY: the `pages` pages at `at` were just allocated to this loader,
        // are identity-mapped while boot services live, and nothing else holds them.
        let into = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, len as usize) };

        println!("ROOT: reading {len} bytes at LBA {}+{blocks}", part.first_lba);
        let began = tsc();
        if let Err(e) = self.io.read_blocks(self.media_id, part.first_lba, into) {
            refuse(format_args!("the read of {len} bytes at LBA {} failed: {e:?}", part.first_lba));
        }
        let took = tsc().wrapping_sub(began);
        println!("{} {at:#x}+{len:#x} in {took} TSC cycles", READ_AT);
        RootImage { at, len }
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

/// The time-stamp counter; the kernel's `TSC:` record converts it.
fn tsc() -> u64 {
    // SAFETY: RDTSC reads a counter and nothing else; every x86-64 has it.
    unsafe { core::arch::x86_64::_rdtsc() }
}
