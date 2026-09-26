//! What a mutating call does when one of its device writes has an unknown
//! outcome.
//!
//! A refused write may already be on the medium: a block layer can issue a
//! write, lose the answer, and report its own budget expired. So a refusal says
//! nothing about which of the two states an entry is in, and nothing here reads
//! to find out — a cache under [`BlockAccess`] may still serve the bytes the
//! refusal did not update. What is known is the state the call wanted and the
//! state it started from, and writing either one is idempotent.
//!
//! **The invariant**: at every point a call can stop, [`Fat32::repair`] holds
//! the writes that take the volume to a consistent state from wherever the
//! call's writes have reached, each an absolute target rather than a delta. A
//! call that returns `Err` re-drives them; one the device refuses stays, and
//! [`Fat32::atomic`] re-drives it before the next mutating call touches
//! anything, so nothing is allocated, freed or linked over an entry whose
//! value on the medium is unknown.
//!
//! Before a call's commit the repair is its rollback, so a refused call is a
//! call that did not happen. The one commit this crate has is a free: once
//! `remove` has erased the entry naming a chain, or a truncation has
//! terminated the kept cluster, the rest of the chain can only go forward, and
//! the repair is the free's remaining walk. That is also what keeps the repair
//! bounded: no step names more than one chain, so its length is a property of
//! the call and never of the file.
//!
//! What stays open, because FAT has no journal:
//!
//! - **Between the two copies of one entry.** [`Fat32::set_fat_entry`] is one
//!   write per copy, so a stop between them leaves one entry's copies
//!   disagreeing, each holding a state the call or its repair passed through.
//!   At most one entry is ever split, because a repair re-drives the refused
//!   entry before any other.
//! - **Between a claim and its link.** A claimed cluster is end-of-chain before
//!   anything reaches it, so a stop there orphans it: a leak, never a chain
//!   into free space.
//! - **Between the entries of one name.** A long name is a run of up to
//!   twenty-one directory entries written one at a time, so a stop inside a
//!   create, rename or remove leaves a partial run.
//! - **The repair lives in memory.** A reset, panic or power loss while it is
//!   non-empty leaves the state the refused write reached: orphaned clusters
//!   bounded by one call's claims, at most one split entry, and at most one
//!   partial run, each of which `toyos-fat32-check` names. Nothing on the
//!   mount path repairs them.

use crate::boot::Cluster;
use crate::device::BlockAccess;
use crate::dir::RawEntry;
use crate::error::Error;
use crate::fs::Fat32;

/// One write that moves the volume towards consistency.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Repair {
    /// `value` into `cluster`'s entry, in every FAT.
    Put { cluster: Cluster, value: u32 },
    /// Free the chain from `start`, which no entry reaches; refused at
    /// `anchor`, the cluster a truncation keeps.
    Free { start: Cluster, anchor: Option<Cluster> },
    /// A directory entry's 32 bytes at `offset`.
    Entry { offset: u64, raw: RawEntry },
}

impl<D: BlockAccess> Fat32<D> {
    /// Run one mutating call so that an `Err` leaves the volume where the
    /// repair takes it.
    ///
    /// Never nested: a call built from other calls uses their bodies, since a
    /// nested success would discard a repair its caller still needs.
    pub(crate) fn atomic<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.settle()?;
        let result = op(self);
        match &result {
            Ok(_) => self.repair.clear(),
            Err(_) if !self.repair.is_empty() => {
                // The count moved on writes whose outcome is now unknown;
                // `sync` counts the FAT again rather than write a guess.
                self.fsinfo.free_count = None;
                self.fsinfo.dirty = true;
                // A refusal here leaves the rest queued for the next call, which
                // cannot start until it lands; the caller learns of it there.
                let _ = self.settle();
            }
            Err(_) => {}
        }
        result
    }

    /// Re-drive every queued repair, the most recent write's first.
    pub(crate) fn settle(&mut self) -> Result<(), Error> {
        while let Some(&step) = self.repair.last() {
            match step {
                Repair::Put { cluster, value } => {
                    self.set_fat_entry(cluster, value)?;
                    self.repair.pop();
                    if value == 0 {
                        self.hint_free(cluster);
                    }
                }
                Repair::Entry { offset, raw } => {
                    self.put_entry_at(offset, &raw)?;
                    self.repair.pop();
                }
                Repair::Free { start, anchor } => {
                    self.repair.pop();
                    self.free_chain(start, anchor)?;
                }
            }
        }
        Ok(())
    }

    /// Start the next allocation's scan at `cluster` if it is lower than
    /// where the scan would start, so a retry re-claims what a refusal gave
    /// back.
    pub(crate) fn hint_free(&mut self, cluster: Cluster) {
        if self.fsinfo.next_free.is_none_or(|n| cluster < n) {
            self.fsinfo.next_free = Some(cluster);
            self.fsinfo.dirty = true;
        }
    }
}
