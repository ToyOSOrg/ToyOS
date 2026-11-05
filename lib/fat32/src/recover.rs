//! What a writable mount takes back of a volume a stop left: the states
//! [`crate::repair`]'s open windows name, and the one a rename stopped
//! between its writes leaves.
//!
//! **Nothing is written unless everything found is one of those states.**
//! The volume is read whole first: every directory walked, every chain
//! followed to its file's size, the FAT copies compared. Anything a stop of
//! this crate cannot leave — a link that is not one, a directory that does
//! not read whole, a cluster two different entries reach, a chain short of
//! its size, FAT copies that differ where one does not read whole — refuses
//! the recovery by name and leaves every byte as it was, since a walk that
//! did not reach a cluster cannot say it is lost. Then, in this order, each
//! write moving towards consistency so a stop inside it leaves no worse a
//! volume:
//!
//! - **A split FAT entry** takes the active copy's value, which the walk
//!   read whole.
//! - **A rename's second name** — an entry equal in every field but the name
//!   to one the walk already met, reaching the same chain — is erased, so the
//!   file keeps its old name or its new one, and each directory's `..` names
//!   the parent its kept entry is in.
//! - **A long-name entry no short entry completes** is freed.
//! - **A file's chain longer than its size** is ended at the size, so the
//!   bytes a growth wrote past its entry are lost, as the entry says.
//! - **A cluster no entry reaches** is freed, and the free count is counted
//!   again.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::boot::Cluster;
use crate::device::Volume as _;
use crate::dir::{DirScan, EntryCursor, RawEntry};
use crate::error::Error;
use crate::fat::{END_OF_CHAIN, ENTRY_MASK};
use crate::fs::{Fat32, Loc};
use toyos_blockdev::BlockDev;

/// What [`Fat32::recover`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Every finding was a stop's, and this is what was written.
    Recovered(Recovered),
    /// The volume holds what no stop of this crate leaves, named here, and
    /// nothing was written.
    Refused(String),
}

/// What a recovery changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    /// Split FAT entries whose mirrors took the active copy's value.
    pub mirrors: u32,
    /// Second names a stopped rename left, erased.
    pub renames: u32,
    /// Directories whose `..` was repointed at the parent their entry is in.
    pub parents: u32,
    /// Long-name entries no short entry completed, freed.
    pub long_names: u32,
    /// Files whose chain was ended at their size; the clusters past it are
    /// counted in [`Self::lost`].
    pub ended: u32,
    /// Allocated clusters no entry reached, freed.
    pub lost: u32,
}

impl Recovered {
    /// Whether the volume was already consistent.
    pub fn nothing(&self) -> bool {
        *self == Self::default()
    }
}

/// Free in a deleted directory entry's first byte.
const DELETED: u8 = 0xE5;
/// The bad-cluster marker, which is allocated and reached by nothing.
const BAD: u32 = 0x0FFF_FFF7;

/// One bit per cluster number: reached by an entry.
struct Reached(Vec<u64>);

impl Reached {
    fn new(clusters: u32) -> Reached {
        Reached(vec![0; (clusters as usize + 2).div_ceil(64)])
    }

    /// Mark `c`; false if it was already marked.
    fn mark(&mut self, c: Cluster) -> bool {
        let (word, bit) = (c.raw() as usize / 64, c.raw() % 64);
        let was = self.0[word] & (1 << bit) != 0;
        self.0[word] |= 1 << bit;
        !was
    }

    fn has(&self, raw: u32) -> bool {
        self.0[raw as usize / 64] & (1 << (raw % 64)) != 0
    }
}

/// What the read-only pass found, every item a stop's.
struct Findings {
    /// Each FAT entry whose copies differ, and the active copy's value.
    splits: Vec<(Cluster, u32)>,
    seconds: Vec<Loc>,
    /// Directories whose `..` does not name their parent, and that parent.
    parents: Vec<(Cluster, Cluster)>,
    /// Offsets of long-name entries no short entry completes.
    names: Vec<u64>,
    /// Each long chain's cluster at its file's size.
    ends: Vec<Cluster>,
    reached: Reached,
}

/// The walk met what no stop leaves.
type Unexplained = String;

impl<D: BlockDev> Fat32<D> {
    /// Take back what a stop left, then sync; or refuse, writing nothing.
    /// For a writable mount, before its first write: the volume must not be
    /// written by anything else meanwhile.
    pub async fn recover(&mut self) -> Result<Recovery, Error> {
        assert!(!self.in_call, "toyos-fat32: recover inside a mutating call");
        self.settle().await?;
        let found = match self.examine().await? {
            Ok(found) => found,
            Err(why) => return Ok(Recovery::Refused(why)),
        };
        let mut done = Recovered::default();
        for &(cluster, value) in &found.splits {
            for fat in self.geom.fat_mirrors().filter(|&f| f != 0) {
                self.write_fat_entry(fat, cluster, value).await?;
            }
            done.mirrors += 1;
        }
        for loc in found.seconds {
            let mark = self.repair.len();
            self.erase_entries(loc).await?;
            self.repair.truncate(mark);
            done.renames += 1;
        }
        for (dir, parent) in found.parents {
            let mark = self.repair.len();
            self.set_dot_dot(dir, parent).await?;
            self.repair.truncate(mark);
            done.parents += 1;
        }
        for offset in found.names {
            let mut raw = self.read_entry_at(offset).await?;
            raw.0[0] = DELETED;
            self.put_entry_at(offset, &raw).await?;
            done.long_names += 1;
        }
        for last in found.ends {
            self.set_fat_entry(last, END_OF_CHAIN).await?;
            done.ended += 1;
        }
        let free;
        (done.lost, free) = self.free_unreached(&found.reached).await?;
        self.fsinfo.free_count = Some(free);
        self.fsinfo.next_free = None;
        self.fsinfo.dirty = true;
        self.sync().await?;
        Ok(Recovery::Recovered(done))
    }

    /// Read the volume whole and say what a recovery would write, or what it
    /// found that no stop leaves.
    ///
    /// **Copies that differ are a stop's only where each reads whole.** A
    /// stopped flush of a write-back device may have written any of its FAT
    /// blocks of either copy; each copy still reads whole because an entry
    /// reaches the disk a flush after its chain, and an erase a flush before
    /// its chain's free, so the blocks differ only past a file's size or in
    /// clusters nothing reaches. A copy torn or written by something else
    /// does not read whole, and then neither is written over.
    async fn examine(&mut self) -> Result<Result<Findings, Unexplained>, Error> {
        let splits = self.splits().await?;
        let mut found = Findings {
            splits,
            seconds: Vec::new(),
            parents: Vec::new(),
            names: Vec::new(),
            ends: Vec::new(),
            reached: Reached::new(self.geom.cluster_count),
        };
        if let Err(why) = self.walk_tree(&mut found).await? {
            return Ok(Err(why));
        }
        if !found.splits.is_empty() {
            for copy in self.geom.fat_mirrors().filter(|&f| f != 0) {
                if let Err(why) = self.walk_copy(copy).await? {
                    let n = found.splits.len();
                    return Ok(Err(format!("the FAT copies differ in {n} entries, and copy {copy} does not read whole: {why}")));
                }
            }
        }
        Ok(Ok(found))
    }

    /// The walk, reading FAT `copy` in place of the active one.
    async fn walk_copy(&mut self, copy: u32) -> Result<Result<(), Unexplained>, Error> {
        let mut found = Findings {
            splits: Vec::new(),
            seconds: Vec::new(),
            parents: Vec::new(),
            names: Vec::new(),
            ends: Vec::new(),
            reached: Reached::new(self.geom.cluster_count),
        };
        let active = self.geom.active_fat.replace(copy);
        let walked = self.walk_tree(&mut found).await;
        self.geom.active_fat = active;
        walked
    }

    /// Every FAT entry whose mirrors differ from FAT 0, with FAT 0's value;
    /// none where mirroring is off, since the other copies are not the
    /// volume's then.
    async fn splits(&mut self) -> Result<Vec<(Cluster, u32)>, Error> {
        let mut splits = Vec::new();
        if self.geom.active_fat.is_some() {
            return Ok(splits);
        }
        let bps = self.geom.bytes_per_sector as usize;
        let sectors = self.geom.max_cluster() as u64 * 4 / bps as u64 + 1;
        let mut ours = vec![0u8; bps];
        let mirrors: Vec<u32> = self.geom.fat_mirrors().filter(|&f| f != 0).collect();
        let mut theirs = vec![vec![0u8; bps]; mirrors.len()];
        for sector in 0..sectors {
            let at = sector * bps as u64;
            self.dev.read_at(self.geom.fat_base_offset(0) + at, &mut ours).await?;
            for (fat, buf) in mirrors.iter().zip(theirs.iter_mut()) {
                self.dev.read_at(self.geom.fat_base_offset(*fat) + at, buf).await?;
            }
            for e in 0..bps / 4 {
                let entry = |b: &[u8]| u32::from_le_bytes(b[e * 4..e * 4 + 4].try_into().expect("4 bytes")) & ENTRY_MASK;
                let number = (sector as usize * bps / 4 + e) as u32;
                let Some(cluster) = self.geom.cluster(number) else { continue };
                if theirs.iter().any(|buf| entry(buf) != entry(&ours)) {
                    splits.push((cluster, entry(&ours)));
                }
            }
        }
        Ok(splits)
    }

    /// Walk the tree from the root, marking every cluster it reaches and
    /// noting what a stop left; the first thing no stop leaves ends it.
    async fn walk_tree(&mut self, found: &mut Findings) -> Result<Result<(), Unexplained>, Error> {
        let whole = self.geom.cluster_count as u64;
        let bpc = self.geom.bytes_per_cluster() as u64;
        let root = self.geom.root();
        if self.reach_chain(root, whole, &mut found.reached).await?.is_err() {
            return Ok(Err("the root directory's chain does not read whole".into()));
        }
        // Each entry's first cluster, and every field of the entry but its
        // name: byte 12, the case of the short name, is the name's.
        let mut named: BTreeMap<u32, [u8; 20]> = BTreeMap::new();
        let mut dirs = vec![root];
        while let Some(dir) = dirs.pop() {
            let mut scan = DirScan::new(dir);
            let mut after = 0u32;
            loop {
                let located = match scan.next(self).await {
                    Ok(located) => located,
                    Err(Error::CorruptDirectory | Error::CorruptChain) => {
                        return Ok(Err(format!("the directory at cluster {} does not read whole", dir.raw())));
                    }
                    Err(e) => return Err(e),
                };
                let end = located.as_ref().map_or(scan.scanned(), |l| l.first_index);
                self.orphan_names(dir, after, end, &mut found.names).await?;
                let Some(located) = located else { break };
                after = located.index + 1;
                let (raw, size) = (located.raw, located.raw.size() as u64);
                let Some(first) = self.geom.cluster(raw.first_cluster()) else {
                    if raw.first_cluster() != 0 || (!raw.is_dir() && size != 0) {
                        return Ok(Err(format!(
                            "an entry in the directory at cluster {} names cluster {} for {size} bytes",
                            dir.raw(),
                            raw.first_cluster()
                        )));
                    }
                    continue;
                };
                let mut fields = [0u8; 20];
                fields[0] = raw.0[11];
                fields[1..].copy_from_slice(&raw.0[13..]);
                if found.reached.has(first.raw()) {
                    if named.get(&first.raw()) != Some(&fields) {
                        return Ok(Err(format!("cluster {} is reached by two different entries", first.raw())));
                    }
                    found.seconds.push(Loc {
                        dir_start: dir,
                        first_index: located.first_index,
                        index: located.index,
                        entry_offset: located.offset,
                    });
                    continue;
                }
                named.insert(first.raw(), fields);
                let unexplained = |what: &str| format!("the chain from cluster {} {what}", first.raw());
                if raw.is_dir() {
                    if self.reach_chain(first, whole, &mut found.reached).await?.is_err() {
                        return Ok(Err(unexplained("of a directory does not read whole")));
                    }
                    match self.names_parent(first, dir).await? {
                        Some(true) => {}
                        Some(false) => found.parents.push((first, dir)),
                        None => return Ok(Err(format!("the directory at cluster {} has no `..`", first.raw()))),
                    }
                    dirs.push(first);
                } else if size == 0 {
                    return Ok(Err(unexplained("is named by an empty file")));
                } else {
                    let keep = size.div_ceil(bpc);
                    match self.reach_chain(first, keep, &mut found.reached).await? {
                        Ok((n, last)) if n == keep => match self.next_cluster(last).await {
                            Ok(Some(_)) => found.ends.push(last),
                            Ok(None) => {}
                            Err(Error::CorruptChain) => return Ok(Err(unexplained("ends in a link that is not one"))),
                            Err(e) => return Err(e),
                        },
                        Ok(_) => return Ok(Err(unexplained(&format!("is short of its file's {size} bytes")))),
                        Err(()) => return Ok(Err(unexplained("of a file does not read whole"))),
                    }
                }
            }
        }
        Ok(Ok(()))
    }

    /// Whether `dir`'s `..` names `parent`; `None` for a directory with no
    /// `..` entry.
    async fn names_parent(&mut self, dir: Cluster, parent: Cluster) -> Result<Option<bool>, Error> {
        let Some(offset) = EntryCursor::new(dir).offset_of(self, 1).await? else { return Ok(None) };
        let raw = self.read_entry_at(offset).await?;
        if raw.0[..2] != *b".." {
            return Ok(None);
        }
        Ok(Some(raw.first_cluster() == self.dotdot_target(parent)))
    }

    /// Mark up to `limit` clusters of the chain from `start`: their count and
    /// the last, or `Err(())` for a chain that reaches a marked cluster or a
    /// link that is not one.
    async fn reach_chain(
        &mut self,
        start: Cluster,
        limit: u64,
        reached: &mut Reached,
    ) -> Result<Result<(u64, Cluster), ()>, Error> {
        let mut c = start;
        let mut n = 0;
        loop {
            if !reached.mark(c) {
                return Ok(Err(()));
            }
            n += 1;
            if n == limit {
                return Ok(Ok((n, c)));
            }
            match self.next_cluster(c).await {
                Ok(Some(next)) => c = next,
                Ok(None) => return Ok(Ok((n, c))),
                Err(Error::CorruptChain) => return Ok(Err(())),
                Err(e) => return Err(e),
            }
        }
    }

    /// Note each live long-name entry of `dir` from index `from` before
    /// `to`: none of them is part of a name a short entry completes.
    async fn orphan_names(&mut self, dir: Cluster, from: u32, to: u32, names: &mut Vec<u64>) -> Result<(), Error> {
        let mut cursor = EntryCursor::new(dir);
        for index in from..to {
            let Some(offset) = cursor.offset_of(self, index).await? else { break };
            let raw: RawEntry = self.read_entry_at(offset).await?;
            if raw.is_lfn() && raw.0[0] != DELETED {
                names.push(offset);
            }
        }
        Ok(())
    }

    /// Free every allocated cluster `reached` does not hold: how many, and
    /// the free clusters then.
    async fn free_unreached(&mut self, reached: &Reached) -> Result<(u32, u32), Error> {
        let bps = self.geom.bytes_per_sector as usize;
        let sectors = self.geom.max_cluster() as u64 * 4 / bps as u64 + 1;
        let (mut lost, mut free) = (0, 0);
        let mut buf = vec![0u8; bps];
        for sector in 0..sectors {
            self.dev.read_at(self.geom.fat_base_offset(self.geom.active_fat.unwrap_or(0)) + sector * bps as u64, &mut buf).await?;
            for e in 0..bps / 4 {
                let number = (sector as usize * bps / 4 + e) as u32;
                let Some(cluster) = self.geom.cluster(number) else { continue };
                let value = u32::from_le_bytes(buf[e * 4..e * 4 + 4].try_into().expect("4 bytes")) & ENTRY_MASK;
                if value == 0 {
                    free += 1;
                } else if value != BAD && !reached.has(number) {
                    self.set_fat_entry(cluster, 0).await?;
                    lost += 1;
                }
            }
        }
        Ok((lost, free + lost))
    }
}
