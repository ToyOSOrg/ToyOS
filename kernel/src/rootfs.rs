//! ROOT: the filesystem the kernel argument names, mounted read-only at `/system`.
//!
//! The boot parameter carries a *filesystem* UUID and not a partition GUID, so
//! a role survives its filesystem moving disks or gaining members. `gpt`
//! collects every TOYOS-ROOT-typed partition; this opens each through its own
//! [`page_cache`] over a `block::Partition` view and keeps the one whose
//! superblock carries the argument's UUID. Every byte read here crossed a trust
//! boundary, so a candidate that will not mount is refused by name and skipped;
//! ending with none, or with more than one, panics naming the UUID and every
//! candidate.

use alloc::sync::Arc;
use alloc::vec::Vec;

use bcachefs::{FsUuid, Mounted, ReadOnly};

use crate::bcachefs_adapter::PageCacheBlockIO;
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
    let mut matched: Vec<(gpt::Candidate, Root)> = Vec::new();
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
fn open(candidate: &gpt::Candidate) -> Option<Root> {
    let cache = page_cache::over_candidate(candidate, "root")?;
    match Mounted::<_, ReadOnly>::open(PageCacheBlockIO::new(Arc::clone(&cache))) {
        Ok(fs) => Some(Root { fs, cache }),
        Err(e) => {
            log!(
                "root: candidate {} on device {} holds no filesystem this kernel can mount: {e:?}",
                candidate.guid,
                candidate.volume.device
            );
            None
        }
    }
}
