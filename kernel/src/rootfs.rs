//! ROOT: the filesystem the kernel argument names, mounted read-only.
//!
//! The boot parameter carries a *filesystem* UUID and not a partition GUID, so
//! a role survives its filesystem moving disks or gaining members. `gpt`
//! collects every TOYOS-ROOT-typed partition; this opens each through its own
//! [`block::Partition`] view and page cache and keeps the one whose superblock
//! carries the argument's UUID. Every byte read here crossed a trust boundary,
//! so a candidate that will not mount is refused by name and skipped; ending
//! with none, or with more than one, panics naming the UUID and every candidate.

use alloc::sync::Arc;
use alloc::vec::Vec;

use bcachefs::{FsUuid, Mounted, ReadOnly};

use crate::bcachefs_adapter::PageCacheBlockIO;
use crate::block;
use crate::gpt;
use crate::page_cache;
use crate::sync::Lock;

/// What the boot parameter named, taken out of it before `mm::init` may hand
/// that pool memory back: nothing may hold a borrow of it this far into a boot.
static NAMED: Lock<Option<FsUuid>> = Lock::new(None);

/// A mounted ROOT and the cache its file backings read through.
pub struct Root {
    pub fs: Mounted<PageCacheBlockIO, ReadOnly>,
    pub cache: Arc<page_cache::Cached>,
}

/// Take ROOT's name out of the boot parameter. Runs before `mm::init`, so it
/// neither allocates nor logs.
pub fn init(cmdline: &str) {
    *NAMED.lock() = toyos_abi::boot::root_uuid(cmdline).and_then(FsUuid::parse);
}

/// Mount the filesystem the boot parameter named. Panics when the machine
/// carries no such filesystem, or more than one.
pub fn mount() -> Root {
    let Some(named) = *NAMED.lock() else {
        panic!("boot: the kernel argument names no root filesystem this kernel can parse");
    };

    let candidates = gpt::root_candidates();
    let mut matched: Vec<(gpt::RootCandidate, Root)> = Vec::new();
    for candidate in &candidates {
        let Some(root) = open(candidate) else { continue };
        if root.fs.uuid() == named {
            matched.push((*candidate, root));
        } else {
            log!(
                "root: device {} partition {} holds the filesystem {}, which is not {named}",
                candidate.volume.device,
                candidate.guid,
                root.fs.uuid()
            );
        }
    }

    if matched.len() != 1 {
        for candidate in &candidates {
            log!(
                "root: candidate {} on device {} at LBA {}+{}",
                candidate.guid,
                candidate.volume.device,
                candidate.volume.start_lba,
                candidate.volume.blocks
            );
        }
        panic!(
            "boot: root={named} matches {} of the {} TOYOS-ROOT partition(s) this machine carries",
            matched.len(),
            candidates.len()
        );
    }

    let (candidate, root) = matched.remove(0);
    log!(
        "root: mounted {named} read-only off device {} at LBA {}+{}, {} blocks",
        candidate.volume.device,
        candidate.volume.start_lba,
        candidate.volume.blocks,
        root.cache.partition().block_count()
    );
    root
}

/// Open one candidate, or `None` with the reason logged.
///
/// The mount is the acceptance test, because it is what applies
/// `Superblock::check`; a candidate that fails it is another disk's or damaged,
/// and either way not this machine's business to panic over.
fn open(candidate: &gpt::RootCandidate) -> Option<Root> {
    let volume = candidate.volume;
    let guid = candidate.guid;
    let Some(handle) = block::open(volume.device) else {
        log!("root: candidate {guid} is on device {} and no driver here registered it", volume.device);
        return None;
    };

    // Every number below is one the disk chose, so each refusal says which.
    let lba = volume.lba_bytes as u64;
    let (Some(start), Some(len)) =
        (volume.start_lba.checked_mul(lba), volume.blocks.checked_mul(lba))
    else {
        log!(
            "root: candidate {guid} claims LBA {}+{} of {lba} bytes, which is not a byte range — \
             refusing it",
            volume.start_lba,
            volume.blocks
        );
        return None;
    };
    // Whole device blocks or nothing: a view that began or ended inside one
    // would share it with the table or with the next partition.
    if start % PAGE != 0 || len % PAGE != 0 {
        log!(
            "root: candidate {guid} is at {start}+{len} bytes, which is not whole {PAGE}-byte \
             blocks — refusing it"
        );
        return None;
    }
    let device_blocks = handle.block_count();
    let Some(part) = block::Partition::of(handle, start / PAGE, len / PAGE) else {
        log!(
            "root: candidate {guid} is at {start}+{len} on a device of {} bytes — refusing to \
             read past the end of it",
            device_blocks.saturating_mul(PAGE)
        );
        return None;
    };

    let cache = page_cache::init(part);
    match Mounted::<_, ReadOnly>::open(PageCacheBlockIO::new(Arc::clone(&cache))) {
        Ok(fs) => Some(Root { fs, cache }),
        Err(e) => {
            log!("root: candidate {guid} on device {} holds no filesystem this kernel can mount: {e:?}", volume.device);
            None
        }
    }
}

/// The block every `BlockDevice` transfers.
const PAGE: u64 = crate::mm::PAGE_SIZE;
