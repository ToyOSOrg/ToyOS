//! What a mutating call does when one of its device reads or writes is
//! refused, and a refused write's outcome is unknown.
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
//! value on the medium is unknown. A call refused because that re-drive was
//! refused again answers [`Error::RepairPending`].
//!
//! Before a call's commit the repair is its rollback, so a refused call is a
//! call that did not happen. The one commit this crate has is a free: once
//! `remove` has erased the entry naming a chain, or a truncation has
//! terminated the kept cluster, the rest of the chain can only go forward, and
//! the repair is the free's remaining walk. A `remove` answers `Ok` from its
//! commit on, because the name is gone; what the device refused of its free
//! stays queued. No step names more than one chain, so the repair's length is
//! a property of the call and never of the file, and [`MAX_REPAIR_STEPS`] is
//! its bound.
//!
//! The free-cluster count stays exact through refusals: a claim counts when it
//! is decided, before its write, and a release — a free, or a claim given back
//! — counts when its write lands. Every queued step is driven until it lands,
//! so no refusal leaves the count a guess.
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
//! - **Inside a free.** The name is erased or the kept cluster terminated
//!   first, so a stop part way through the walk orphans the rest of the chain.
//! - **Between the entries of one name.** A long name is a run of up to
//!   twenty-one directory entries written one at a time, so a stop inside a
//!   create, rename or remove leaves a partial run.
//! - **The repair lives in memory.** A reset, panic or power loss while it is
//!   non-empty leaves the state the refused write reached: orphaned clusters
//!   bounded by what one call claims and frees, at most one split entry, and
//!   at most one partial run, each of which `toyos-fat32-check` names. Nothing
//!   on the mount path repairs them.

use crate::boot::Cluster;
use crate::device::BlockAccess;
use crate::dir::RawEntry;
use crate::error::Error;
use crate::fs::Fat32;
use crate::name::MAX_LFN_ENTRIES;

/// The entries of one name: its long-name run and its short entry.
const NAME_ENTRIES: usize = MAX_LFN_ENTRIES + 1;

/// Directory entries in the smallest cluster FAT32 allows, 512 bytes.
const MIN_CLUSTER_ENTRIES: usize = 512 / 32;

/// The most steps a repair holds: a rename's inserted run, the directory
/// growths that made room for it — a claim and a link each — its erased run,
/// and the moved directory's `..`.
pub const MAX_REPAIR_STEPS: usize =
    2 * NAME_ENTRIES + 2 * NAME_ENTRIES.div_ceil(MIN_CLUSTER_ENTRIES) + 1;

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

/// The [`Fat32::repair_episode`] a caller last announced, so a log names each
/// pending repair once rather than at every call that meets it.
#[derive(Debug, Default)]
pub struct RepairNotice(Option<u64>);

impl RepairNotice {
    /// Whether a call answering `e` while `episode` ([`Fat32::repair_episode`])
    /// is queued leaves the volume waiting on that repair: `RepairPending` is a
    /// budget refusing its re-drive, and `Io` a device failure refusing either
    /// the re-drive or the call's own write, which queues it the same way.
    pub fn waits_on(e: Error, episode: Option<u64>) -> bool {
        episode.is_some() && matches!(e, Error::RepairPending | Error::Io)
    }

    /// Whether `episode` is a pending repair not yet announced; from here it
    /// is. `None`, nothing pending, is never one.
    pub fn first_sight(&mut self, episode: Option<u64>) -> bool {
        if episode.is_none() || episode == self.0 {
            return false;
        }
        self.0 = episode;
        true
    }
}

impl<D: BlockAccess> Fat32<D> {
    /// Run one mutating call so that an `Err` leaves the volume where the
    /// repair takes it.
    ///
    /// A call inside another panics: the inner one's success would discard a
    /// repair its caller still needs, so a call built from other calls uses
    /// their bodies.
    pub(crate) fn atomic<T>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        assert!(!self.in_call, "toyos-fat32: a mutating call nested inside another");
        self.settle_first()?;
        self.in_call = true;
        let result = op(self);
        self.in_call = false;
        let committed = core::mem::take(&mut self.committed);
        let answer = match result {
            // What the repair holds past a commit is the call's own remaining
            // work, driven twice like a call and its re-drive; a step the
            // device still refuses stays queued for the next call to finish
            // and report.
            Ok(v) if committed => {
                let mut driven = self.settle();
                if matches!(driven, Err(Error::BudgetExpired | Error::Io)) {
                    driven = self.settle();
                }
                match driven {
                    Ok(()) | Err(Error::BudgetExpired | Error::Io) => Ok(v),
                    Err(e) => Err(e),
                }
            }
            Ok(v) => {
                self.repair.clear();
                Ok(v)
            }
            Err(e) if self.repair.is_empty() => Err(e),
            // An unlanded re-drive is what the volume is waiting on, so it is
            // the answer rather than the error that started the rollback.
            Err(e) => self.settle().map_or_else(Err, |()| Err(e)),
        };
        // The queue was empty when `settle_first` let this call start.
        if !self.repair.is_empty() {
            self.episodes += 1;
        }
        answer
    }

    /// Mark the call committed: its rollback is discarded, and what it queues
    /// from here is carried forward even when it answers `Ok`.
    pub(crate) fn commit(&mut self) {
        self.repair.clear();
        self.committed = true;
    }

    /// Queue one step, most recent last.
    pub(crate) fn queue(&mut self, step: Repair) {
        assert!(
            self.repair.len() < MAX_REPAIR_STEPS,
            "toyos-fat32: a repair past its bound of {MAX_REPAIR_STEPS} steps: {step:?}"
        );
        self.repair.push(step);
    }

    /// Steps a refused call left queued, which no mutating call can start
    /// before. Zero once every write has landed.
    pub fn pending_repair(&self) -> usize {
        self.repair.len()
    }

    /// Which refused call's repair is queued, numbered from mount, or `None`
    /// when nothing is. Two answers that differ are two refused calls, whatever
    /// landed between them.
    pub fn repair_episode(&self) -> Option<u64> {
        (!self.repair.is_empty()).then_some(self.episodes)
    }

    /// [`Self::settle`] before a call of its own, naming a refusal as the
    /// earlier call's repair rather than this call's write.
    ///
    /// Every other error it answers is the earlier call's too, answered by a
    /// call that did not start: a device failure, or a
    /// [`Error::CorruptChain`] met walking a queued free, whose chain past the
    /// corrupt link leaks and is recorded nowhere else.
    pub(crate) fn settle_first(&mut self) -> Result<(), Error> {
        self.settle().map_err(|e| match e {
            Error::BudgetExpired => Error::RepairPending,
            e => e,
        })
    }

    /// Re-drive every queued repair, the most recent write's first.
    pub(crate) fn settle(&mut self) -> Result<(), Error> {
        while let Some(&step) = self.repair.last() {
            match step {
                Repair::Put { cluster, value } => {
                    self.set_fat_entry(cluster, value)?;
                    self.repair.pop();
                    if value == 0 {
                        self.released(cluster);
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

    /// Count a cluster whose free has landed, and start the next allocation's
    /// scan at it if that is lower than where the scan would start, so a retry
    /// re-claims what a refusal gave back.
    pub(crate) fn released(&mut self, cluster: Cluster) {
        self.fsinfo.free_count = self.fsinfo.free_count.map(|n| n.saturating_add(1));
        self.fsinfo.dirty = true;
        if self.fsinfo.next_free.is_none_or(|n| cluster < n) {
            self.fsinfo.next_free = Some(cluster);
        }
    }
}
