//! ROOT: the filesystem the kernel argument names, mounted read-only.
//!
//! The boot parameter carries a *filesystem* UUID, not a partition GUID, so a
//! role survives a filesystem moving disks or gaining members. `gpt` collects
//! every TOYOS-ROOT-typed partition it sees; this opens each one through its own
//! [`block::Partition`] view and page cache and keeps the one whose superblock
//! carries the argument's UUID.
//!
//! Every byte read here crossed a trust boundary, so a candidate that will not
//! mount is refused by name and skipped. Ending with none, or with more than
//! one, is a machine this kernel cannot run: it panics naming the UUID and every
//! candidate it saw.

use alloc::sync::Arc;
use alloc::vec::Vec;

use bcachefs::{FsUuid, Mounted, ReadOnly};

use crate::bcachefs_adapter::PageCacheBlockIO;
use crate::block;
use crate::gpt;
use crate::page_cache;
use crate::sync::Lock;

/// What the boot parameter named, taken out of it before `mm::init` may hand
/// that memory back — `KernelArgs::cmdline_addr` is pool memory in no reserved
/// region, so nothing may hold a borrow of it this far into the boot.
static NAMED: Lock<Option<FsUuid>> = Lock::new(None);

/// A mounted ROOT and the cache its file backings read through.
pub struct Root {
    pub fs: Mounted<PageCacheBlockIO, ReadOnly>,
    pub cache: Arc<page_cache::Cached>,
}

/// Take ROOT's name out of the boot parameter. Before `mm::init`, beside
/// `actuator::init`, and it neither allocates nor logs for that reason.
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
/// The mount is the acceptance test: it is what applies `Superblock::check`,
/// which refuses a superblock describing a device other than the one it came
/// off. A candidate that fails any of it is another disk's, or damaged, and
/// either way not this machine's business to panic over.
fn open(candidate: &gpt::RootCandidate) -> Option<Root> {
    let volume = candidate.volume;
    let guid = candidate.guid;
    let Some(handle) = block::open(volume.device) else {
        log!("root: candidate {guid} is on device {} and no driver here registered it", volume.device);
        return None;
    };

    let lba = volume.lba_bytes as u64;
    let start = volume.start_lba.checked_mul(lba)?;
    let len = volume.blocks.checked_mul(lba)?;
    // Whole device blocks or nothing, as every other partition view: one that
    // began or ended inside a device block would share it with the table or
    // with the next partition.
    if start % PAGE != 0 || len % PAGE != 0 {
        log!(
            "root: candidate {guid} is at {start}+{len} bytes, which is not whole {PAGE}-byte \
             blocks — refusing it"
        );
        return None;
    }
    let part = block::Partition::of(handle, start / PAGE, len / PAGE)?;

    let cache = page_cache::init(part);
    match Mounted::<_, ReadOnly>::open(PageCacheBlockIO::new(Arc::clone(&cache))) {
        Ok(fs) => Some(Root { fs, cache }),
        Err(e) => {
            log!("root: candidate {guid} on device {} holds no filesystem this kernel can mount: {e:?}", volume.device);
            None
        }
    }
}

/// `mm::PAGE_SIZE`, which is the block every `BlockDevice` here transfers.
const PAGE: u64 = crate::mm::PAGE_SIZE;
