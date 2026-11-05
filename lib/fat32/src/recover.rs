//! What a writable mount takes back of a volume a stop left: the states
//! [`crate::repair`]'s open windows name, made consistent before the first
//! write. Each step moves towards consistency on its own, so a stop inside
//! this leaves no worse a volume than it found.
//!
//! - **A split FAT entry** takes the active copy's value, which holds a state
//!   the stopped call or its repair passed through.
//! - **A file's chain longer than its size** is ended at the size, so the
//!   bytes a growth wrote past its entry are lost, as the entry says.
//! - **A long-name entry no short entry completes** is freed.
//! - **A cluster no entry reaches** is freed.
//! - **The free count** is counted again.
//!
//! What a stop cannot leave and this cannot settle — a cluster two entries
//! reach, a link that is not one, a chain shorter than its size — is counted
//! as [`Recovered::unrepaired`] and left as it is.

use alloc::vec;
use alloc::vec::Vec;

use crate::boot::Cluster;
use crate::device::Volume as _;
use crate::dir::{DirScan, EntryCursor, RawEntry};
use crate::error::Error;
use crate::fat::{END_OF_CHAIN, ENTRY_MASK};
use crate::fs::Fat32;
use toyos_blockdev::BlockDev;

/// What [`Fat32::recover`] changed, and what it found and left.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    /// FAT entries whose mirrors took the active copy's value.
    pub mirrors: u32,
    /// Files whose chain was ended at their size; what it held past that is
    /// counted in [`Self::lost`].
    pub cut: u32,
    /// Long-name entries no short entry completed, freed.
    pub long_names: u32,
    /// Allocated clusters no entry reached, freed.
    pub lost: u32,
    /// Inconsistencies left as they were.
    pub unrepaired: u32,
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

impl<D: BlockDev> Fat32<D> {
    /// Take back what a stop left, then sync. For a writable mount, before
    /// its first write: the volume must not be written by anything else
    /// meanwhile.
    pub async fn recover(&mut self) -> Result<Recovered, Error> {
        assert!(!self.in_call, "toyos-fat32: recover inside a mutating call");
        self.settle().await?;
        let mut done = Recovered { mirrors: self.reconcile_mirrors().await?, ..Recovered::default() };
        let mut reached = Reached::new(self.geom.cluster_count);
        self.reach_tree(&mut reached, &mut done).await?;
        let free;
        (done.lost, free) = self.free_unreached(&reached).await?;
        self.fsinfo.free_count = Some(free);
        self.fsinfo.next_free = None;
        self.fsinfo.dirty = true;
        self.sync().await?;
        Ok(done)
    }

    /// Every mirror's entry that differs from the active FAT's takes its
    /// value.
    async fn reconcile_mirrors(&mut self) -> Result<u32, Error> {
        // Mirroring off: the other copies are not the volume's.
        if self.geom.active_fat.is_some() {
            return Ok(0);
        }
        let active = 0;
        let bps = self.geom.bytes_per_sector as usize;
        let sectors = self.geom.max_cluster() as u64 * 4 / bps as u64 + 1;
        let mut fixed = 0;
        let mut ours = vec![0u8; bps];
        let mut theirs = vec![0u8; bps];
        self.invalidate_sector();
        for sector in 0..sectors {
            let at = sector * bps as u64;
            self.dev.read_at(self.geom.fat_base_offset(active) + at, &mut ours).await?;
            for fat in self.geom.fat_mirrors().filter(|&f| f != active) {
                self.dev.read_at(self.geom.fat_base_offset(fat) + at, &mut theirs).await?;
                for e in 0..bps / 4 {
                    let entry = |b: &[u8]| u32::from_le_bytes(b[e * 4..e * 4 + 4].try_into().expect("4 bytes")) & ENTRY_MASK;
                    let number = (sector as usize * bps / 4 + e) as u32;
                    let Some(cluster) = self.geom.cluster(number) else { continue };
                    let value = entry(&ours);
                    if entry(&theirs) != value {
                        self.write_fat_entry(fat, cluster, value).await?;
                        fixed += 1;
                    }
                }
            }
        }
        Ok(fixed)
    }

    /// Mark every cluster the tree reaches from the root, freeing each
    /// long-name entry no short entry completes, then end each file's chain
    /// at its size: the rest is reached by nothing, and freed with the lost.
    async fn reach_tree(&mut self, reached: &mut Reached, done: &mut Recovered) -> Result<(), Error> {
        let whole = self.geom.cluster_count as u64;
        let bpc = self.geom.bytes_per_cluster() as u64;
        let mut dirs = Vec::new();
        let mut long = Vec::new();
        let root = self.geom.root();
        match self.reach_chain(root, whole, reached).await? {
            Ok(_) => dirs.push(root),
            Err(()) => done.unrepaired += 1,
        }
        while let Some(dir) = dirs.pop() {
            let mut scan = DirScan::new(dir);
            let mut after = 0u32;
            loop {
                let found = match scan.next(self).await {
                    Ok(found) => found,
                    Err(Error::CorruptDirectory | Error::CorruptChain) => {
                        done.unrepaired += 1;
                        break;
                    }
                    Err(e) => return Err(e),
                };
                let end = found.as_ref().map_or(scan.scanned(), |l| l.first_index);
                done.long_names += self.free_orphan_names(dir, after, end).await?;
                let Some(located) = found else { break };
                after = located.index + 1;
                let (raw, size) = (located.raw, located.raw.size() as u64);
                let Some(first) = self.geom.cluster(raw.first_cluster()) else {
                    if raw.first_cluster() != 0 || (!raw.is_dir() && size != 0) {
                        done.unrepaired += 1;
                    }
                    continue;
                };
                if raw.is_dir() {
                    match self.reach_chain(first, whole, reached).await? {
                        Ok(_) => dirs.push(first),
                        Err(()) => done.unrepaired += 1,
                    }
                } else if size == 0 {
                    // An empty file names no cluster.
                    let mut emptied = raw;
                    emptied.set_first_cluster(0);
                    self.put_entry_at(located.offset, &emptied).await?;
                    done.cut += 1;
                } else {
                    match self.reach_chain(first, size.div_ceil(bpc), reached).await? {
                        Ok((n, last)) if n == size.div_ceil(bpc) => long.push(last),
                        _ => done.unrepaired += 1,
                    }
                }
            }
        }
        for last in long {
            if self.next_cluster(last).await?.is_some() {
                self.set_fat_entry(last, END_OF_CHAIN).await?;
                done.cut += 1;
            }
        }
        Ok(())
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

    /// Free each long-name entry of `dir` from index `from` before `to`:
    /// none of them is part of a name a short entry completes.
    async fn free_orphan_names(&mut self, dir: Cluster, from: u32, to: u32) -> Result<u32, Error> {
        let mut freed = 0;
        let mut cursor = EntryCursor::new(dir);
        for index in from..to {
            let Some(offset) = cursor.offset_of(self, index).await? else { break };
            let mut raw: RawEntry = self.read_entry_at(offset).await?;
            if raw.is_lfn() && raw.0[0] != DELETED {
                raw.0[0] = DELETED;
                self.put_entry_at(offset, &raw).await?;
                freed += 1;
            }
        }
        Ok(freed)
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
