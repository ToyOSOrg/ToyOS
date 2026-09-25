//! ROOT: the filesystem the boot parameter names, mounted read-only at
//! `/system` from the image the loader read into memory.
//!
//! The loader picks the partition and reads its filesystem whole; this kernel
//! reads ROOT from nowhere else. [`init`] takes the image out of the handoff
//! and keeps it only when `toyos_rootimage::handoff` finds it whole blocks
//! inside one `LoaderData` descriptor of firmware's map, returning the region
//! `mm::init` keeps out of the allocator. [`mount`] refuses the boot by name on
//! a handoff with no image, and on an image whose superblock is not the one
//! `root=` names. [`hold_source`] holds the partition the image came from once
//! the disks are up, so no claim writes the slot this boot runs, and refuses
//! the boot when it cannot say that no claim will. The image's bytes crossed a
//! trust boundary like any disk's, so every read of it is bounds-checked and a
//! block outside it is a refused read, never a panic.

use bcachefs::{BlockBuf, BlockIO, BlockNum, DeviceError, FsUuid, Mounted, ReadOnly};
use toyos_abi::boot::{KernelArgs, MemoryMapEntry};
use toyos_rootimage::handoff::{held, Descriptor};

use crate::block::{BlockError, Holder, Partition};
use crate::device::ClaimError;
use crate::mm::{DirectMap, Region};
use crate::sync::Lock;

/// The unit ROOT's filesystem is written in, which is the page.
const BLOCK: usize = crate::mm::PAGE_SIZE as usize;

/// The record init's spawn is reported on, followed by how many storage
/// commands the boot had issued by then: zero is the claim.
pub const INIT_WITHOUT_A_DISK: &str = "boot: init spawned with ROOT from memory; storage commands before it:";

/// The record a ROOT mounted off the loader's image is reported on.
pub const MOUNTED_FROM_MEMORY: &str = "root: mounted read-only from memory at";

/// What the handoff said about ROOT, decided before `mm::init` may hand the
/// loader's pool memory back: nothing may hold a borrow of it this far into a boot.
#[derive(Clone, Copy)]
enum Handed {
    /// The loader handed no image.
    Nothing,
    /// An extent that is not whole blocks inside one `LoaderData` descriptor.
    Unheld { at: u64, len: u64 },
    Image(MemoryImage),
}

#[derive(Clone, Copy)]
struct Boot {
    named: Option<FsUuid>,
    handed: Handed,
    /// The partition the image was read from, raw as in its GPT entry.
    source: [u8; 16],
}

static BOOT: Lock<Boot> = Lock::new(Boot { named: None, handed: Handed::Nothing, source: [0; 16] });

/// The kernel's hold on the partition ROOT was read from, for the machine's life.
static SOURCE: Lock<Option<Partition>> = Lock::new(None);

/// ROOT's bytes: whole blocks, read-only, in memory the kernel never frees.
#[derive(Clone, Copy)]
pub struct MemoryImage(&'static [u8]);

impl MemoryImage {
    /// Where the image is, as the physical address and length the loader handed.
    pub fn extent(&self) -> (u64, u64) {
        (DirectMap::phys_of(self.0.as_ptr()), self.0.len() as u64)
    }

    /// Copy block `block` into `into`; a block outside the image is a refused read.
    pub fn read(&self, block: u64, into: &mut [u8; BLOCK]) -> Result<(), BlockError> {
        let bytes = usize::try_from(block)
            .ok()
            .and_then(|block| block.checked_mul(BLOCK))
            .and_then(|at| self.0.get(at..at.checked_add(BLOCK)?))
            .ok_or(BlockError::Device)?;
        into.copy_from_slice(bytes);
        Ok(())
    }
}

impl BlockIO for MemoryImage {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        self.read(block.raw(), &mut buf.0).map_err(|e| DeviceError::classify(&e))
    }

    /// A `ReadOnly` mount has no path to this; reaching it is a kernel bug.
    fn write_block(&self, block: BlockNum, _buf: &BlockBuf) -> Result<(), DeviceError> {
        panic!("rootfs: a read-only mount wrote block {} of the in-memory ROOT", block.raw());
    }

    fn block_count(&self) -> u64 {
        (self.0.len() / BLOCK) as u64
    }
}

/// Take ROOT's name out of the boot parameter and its image out of the
/// handoff, returning the region `mm::init` must keep: the image, or an empty
/// one where there is none to keep. Runs before `mm::init`, so it neither
/// allocates nor logs.
pub fn init(cmdline: &str, args: &KernelArgs, map: &[MemoryMapEntry]) -> Region {
    let (at, len) = (args.root_image_addr, args.root_image_len);
    let descriptors = map.iter().map(|entry| Descriptor { ty: entry.uefi_type, start: entry.start, end: entry.end });
    let none = Region { start: 0, end: 0 };
    let (handed, region) = match held(descriptors, crate::mm::pmm::EFI_LOADER_DATA, at, len, BLOCK as u64) {
        _ if len == 0 => (Handed::Nothing, none),
        None => (Handed::Unheld { at, len }, none),
        Some(extent) => (
            // SAFETY: the extent is whole pages of `LoaderData` the loader
            // allocated and wrote before the jump, and the region returned here
            // keeps `mm::init`'s allocator off it, so it is live, never written,
            // and mapped at PHYS_OFFSET for the kernel's life.
            Handed::Image(MemoryImage(unsafe {
                core::slice::from_raw_parts(DirectMap::from_phys(at).as_ptr::<u8>(), len as usize)
            })),
            Region { start: extent.start, end: extent.end },
        ),
    };
    *BOOT.lock() = Boot {
        named: toyos_abi::boot::root_uuid(cmdline).and_then(FsUuid::parse),
        handed,
        source: args.root_partition_guid,
    };
    region
}

/// Mount the filesystem the boot parameter named, off the image the loader
/// handed. Panics when there is no image, or it is not that filesystem.
pub fn mount() -> Mounted<MemoryImage, ReadOnly> {
    let Boot { named, handed, .. } = *BOOT.lock();
    let Some(named) = named else {
        panic!("boot: the kernel argument names no root filesystem this kernel can parse");
    };
    let image = match handed {
        Handed::Nothing => panic!(
            "boot: the loader handed no ROOT image, and this kernel reads ROOT from memory and nowhere else"
        ),
        Handed::Unheld { at, len } => panic!(
            "boot: the ROOT image at {at:#x}+{len:#x} is not whole {BLOCK}-byte blocks inside one \
             LoaderData descriptor"
        ),
        Handed::Image(image) => image,
    };
    // The actuator's other half: a loader that ignored it would leave the
    // refusal above untested while the test reading it passed.
    if crate::actuator::loader_withholds_root() {
        panic!(
            "boot: {} is armed and the loader handed a ROOT image anyway",
            toyos_abi::boot::WITHHOLD_ROOT_PARAM
        );
    }
    let fs = Mounted::<_, ReadOnly>::open(image)
        .unwrap_or_else(|e| panic!("boot: the ROOT image holds no filesystem this kernel can mount: {e:?}"));
    if fs.uuid() != named {
        panic!("boot: root={named} and the ROOT image the loader handed holds {}", fs.uuid());
    }
    let (at, len) = image.extent();
    log!("{MOUNTED_FROM_MEMORY} {at:#x}+{len:#x}, filesystem {named}, {} blocks", image.block_count());
    fs
}

/// Hold the partition ROOT was read from, so no process's claim writes the
/// slot this boot is running. Runs once the disks are probed.
///
/// A partition on no disk this kernel drives is one no claim can write either,
/// and one carried twice is one every claim is refused as carried twice, since
/// the disks a claim looks on only grow and no claim holds a table. Every other
/// answer leaves a later claim free to find the partition, so it refuses the
/// boot.
pub fn hold_source() {
    let guid = toyos_gpt::Guid(BOOT.lock().source);
    let found = match crate::gpt::claimable(toyos_abi::part::PartGuid(guid.0)) {
        Ok(found) => found,
        Err(e @ (ClaimError::Absent | ClaimError::Ambiguous)) => {
            log!("root: the partition ROOT was read from, {guid}, is claimable by no one ({e:?})");
            return;
        }
        Err(e) => panic!("boot: the partition ROOT was read from, {guid}, cannot be held: {e:?}"),
    };
    let volume = found.volume;
    let view = crate::block::open(volume.device)
        .ok_or(())
        .and_then(|handle| {
            let (first, blocks) =
                crate::block::span_blocks(volume.start_lba, volume.blocks, volume.lba_bytes)
                    .map_err(drop)?;
            Partition::of(handle, first, blocks, Holder::Kernel("system")).map_err(drop)
        });
    match view {
        Ok(view) => {
            log!("root: holding {}, the partition ROOT was read from, on device {}", found.unique, volume.device);
            *SOURCE.lock() = Some(view);
        }
        // The same refusals a claim of it meets in `device::partition_view`,
        // so a partition this cannot hold is one no claim can take either.
        Err(()) => log!(
            "root: the partition ROOT was read from, {}, is on device {} and is no span a view can hold",
            found.unique, volume.device
        ),
    }
}
