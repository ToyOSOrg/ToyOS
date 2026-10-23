//! ToyOS's check of a volume ToyOS wrote: every invariant the writer
//! maintains, grown with each stage that adds one. It is built from the
//! same facts as the writer, and the allocator and accounting state it
//! expects comes from the same derivation mkfs writes (`derive`), so it
//! judges ToyOS's consistency with itself; upstream's fsck is the
//! independent judge, and an upstream volume's own derived btrees are
//! that derivation's.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use toyos_blockdev::{BlockDev, BLOCK};

use super::bkey::{Bpos, Key};
use super::derive::{Space, Tally};
use super::mkfs::{self, COMPAT_CLEAN, COMPAT_INITIALIZED, DOWNGRADE, EXTENT_TYPE_U64S, FEATURES};
use super::read::{hash, read_bytes, Volume};
use super::superblock::{field_type, member_flag, Superblock, HEADER};
use super::value::{self, data_type, inode_field, inode_flag, AllocV4, Dirent, DirentType, Extent, InodeValue, NodePtr, Subvolume, SUBVOLUME_LIVE};
use super::{kind, le64, Btree, Result, SECTOR, VERSION};

/// One way a volume departs from what ToyOS writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finding {
    /// A superblock rule ToyOS writes by, by name.
    Superblock(&'static str),
    /// A superblock copy that differs from the chosen one in more than its
    /// own offset and checksum, by its sector.
    Copy(u64),
    /// The root subvolume, snapshot, tree, directory or inode cursor
    /// missing or not as mkfs writes them, by name.
    Root(&'static str),
    /// A node outside the member's btree bitmap.
    Bitmap { btree: Btree, sector: u64 },
    /// A bset stamped with a journal sequence past `clean`'s.
    BsetAboveClean { btree: Btree, sector: u64, seq: u64 },
    /// An inode stamped with a journal sequence past `clean`'s.
    InodeAboveClean { ino: u64, seq: u64 },
    /// A key of a type its btree, at that level, does not hold. The reader
    /// passes the non-strict types; ToyOS writes none out of place.
    KeyType { btree: Btree, level: u8, kind: u8, pos: Bpos },
    /// A node whose children do not tile its range, or a root that does not
    /// span every position, by rule.
    Topology { btree: Btree, sector: u64, rule: &'static str },
    /// A key breaking a rule of its btree or its type, by rule.
    Key { btree: Btree, pos: Bpos, rule: &'static str },
    /// A pointer to sectors no one bucket of the device holds, by the
    /// position of the key holding it.
    Pointer { btree: Btree, pos: Bpos, rule: &'static str },
    /// A bucket its pointers fill inconsistently.
    Bucket { bucket: u64, rule: &'static str },
    /// An inode its keys and names disagree with, by rule.
    Inode { ino: u64, rule: &'static str },
    /// A dirent whose directory, target or slot is wrong, by rule.
    Dirent { dir: u64, offset: u64, rule: &'static str },
    /// A key of a derived btree that the volume's keys do not derive
    /// (`unexpected`), that they derive and it lacks (`missing`), or that
    /// they derive with another value (`differs`).
    Derived { btree: Btree, pos: Bpos, rule: &'static str },
}

/// Everything wrong with `volume`; empty when nothing is.
pub async fn fsck<D: BlockDev>(volume: &Volume<D>) -> Result<Vec<Finding>> {
    let sb = volume.superblock();
    let mut found = superblock(sb);
    found.extend(copies(volume.dev(), sb).await?);
    found.extend(root_world(volume).await?);
    let space = Space::of(sb)?;
    let mut walk = Walk::new(&space, sb);
    walk.nodes(volume).await?;
    found.append(&mut walk.findings);
    found.extend(walk.namespace(volume.root(), sb));
    found.extend(walk.derived());
    Ok(found)
}

/// What every ToyOS-written superblock carries.
fn superblock(sb: &Superblock) -> Vec<Finding> {
    let h = &sb.header;
    let clean = sb.clean_flag();
    let ext = sb.field(field_type::EXT).unwrap_or(&[]);
    let flags = sb.member.flags;
    let expected_features = FEATURES | if sb.blacklist.is_empty() { 0 } else { 1 << 5 };
    let expected_compat = COMPAT_INITIALIZED | if clean { COMPAT_CLEAN } else { 0 };
    let has_replicas = |t: u8| sb.replicas.iter().any(|r| r.data_type == t && r.devs == [0] && r.required == 1);
    let padded = |kind: u32, want: &[u8]| {
        sb.field(kind).is_some_and(|b| b.len() >= want.len() && b[..want.len()] == *want && b[want.len()..].iter().all(|&x| x == 0))
    };
    let rules: [(bool, &'static str); 26] = [
        (h.version == VERSION && h.version_min == VERSION, "version and minimum version 1.39"),
        (sb.flag(5, 32, 16) == 1 << 10 | 36, "version incompat 1.36"),
        (sb.flag(5, 48, 16) == u64::from(VERSION), "version incompat allowed 1.39"),
        (sb.flag(5, 0, 16) == u64::from(VERSION), "version upgrade complete 1.39"),
        (ext.len() >= 80 && ext[..80].iter().all(|&b| b == 0), "no recovery pass required and no error silenced"),
        (sb.flag(0, 0, 1) == 1, "initialized"),
        (sb.flag(0, 60, 2) == 0, "no errors or topology errors recorded"),
        (sb.flag(6, 0, 4) == 0, "inode shard bits 0"),
        (sb.flag(6, 40, 8) == 16, "extent backpointer shift 16"),
        (sb.flag(1, 14, 6) == 9, "encoded extent maximum 256 KiB"),
        (sb.flag(0, 40, 4) == 1 && sb.flag(0, 44, 4) == 1, "crc32c metadata and data checksum options"),
        (sb.flag(1, 10, 4) <= 1 && (sb.flag(1, 10, 4) == 0 || sb.flag(1, 9, 1) == 1), "encrypted only as ChaCha20/Poly1305 with wide_macs"),
        (h.features == [expected_features, 0], "format's feature set"),
        (h.compat == [expected_compat, 0], "compat bits 2-5, and 0-1 exactly when clean"),
        (has_replicas(data_type::BTREE), "btree replicas entry"),
        (has_replicas(data_type::JOURNAL) != clean, "journal replicas entry exactly while dirty"),
        (flags & member_flag::FREESPACE_INITIALIZED != 0 && flags & member_flag::INIT_STATE == 0, "member freespace initialized"),
        (flags & member_flag::DISCARD == 0, "member discard off"),
        (flags & member_flag::ROTATIONAL == 0 && flags & member_flag::ROTATIONAL_KNOWN != 0, "member not rotational, and known"),
        (sb.member.identity_zero, "member device name, model and serial zero"),
        (padded(field_type::DOWNGRADE, &DOWNGRADE), "downgrade field of 1.39 above 1.36"),
        (padded(field_type::EXTENT_TYPE_U64S, &EXTENT_TYPE_U64S), "extent entry sizes of 1.39"),
        (sb.field(field_type::COUNTERS).is_none(), "no counters field"),
        (sb.field(field_type::ERRORS_V2).is_none(), "no errors_v2 field"),
        (sb.field(field_type::RECOVERY_PASSES).is_none(), "no recovery_passes field"),
        (!clean || sb.clean.as_ref().is_some_and(|c| c.key_version.is_some()), "clean carries the key version"),
    ];
    rules.iter().filter(|(ok, _)| !ok).map(|&(_, rule)| Finding::Superblock(rule)).collect()
}

/// Every copy is the chosen one's bytes but for its own offset and
/// checksum.
async fn copies(dev: &impl BlockDev, sb: &Superblock) -> Result<Vec<Finding>> {
    let len = HEADER + 8 * sb.fields.iter().map(|f| 1 + f.body.len().div_ceil(8)).sum::<usize>();
    let reference = Superblock::encode(&sb.header, &sb.fields, 0);
    let mut found = Vec::new();
    let device = dev.blocks() * BLOCK as u64;
    for &offset in &sb.header.layout.offsets {
        let fits = offset.checked_mul(SECTOR as u64).and_then(|at| at.checked_add(len as u64)).is_some_and(|end| end <= device);
        if !fits {
            found.push(Finding::Copy(offset));
            continue;
        }
        let mut copy = read_bytes(dev, offset * SECTOR as u64, len).await?;
        copy[..16].fill(0);
        copy[104..112].fill(0);
        let mut want = reference.clone();
        want[..16].fill(0);
        if copy != want {
            found.push(Finding::Copy(offset));
        }
    }
    Ok(found)
}

/// The root world, as mkfs writes it.
async fn root_world<D: BlockDev>(v: &Volume<D>) -> Result<Vec<Finding>> {
    let mut found = Vec::new();
    let mut check = |ok: bool, what| {
        if !ok {
            found.push(Finding::Root(what));
        }
    };
    let snapshot = mkfs::ROOT_SNAPSHOT;
    let subvolume = v.get(Btree::Subvolumes, Bpos::new(0, u64::from(mkfs::ROOT_SUBVOLUME), 0)).await?;
    let subvolume = subvolume.filter(|(k, _)| k.kind == kind::SUBVOLUME).map(|(_, v)| Subvolume::decode(&v)).transpose()?;
    check(
        subvolume.is_some_and(|s| s.snapshot == snapshot && s.inode == mkfs::ROOT_INODE && s.state == SUBVOLUME_LIVE && s.fs_path_parent == 0),
        "root subvolume",
    );
    let tree = v.get(Btree::Snapshots, Bpos::new(0, u64::from(snapshot), 0)).await?;
    check(
        tree.is_some_and(|(k, val)| k.kind == kind::SNAPSHOT && val == value::snapshot(mkfs::ROOT_SUBVOLUME, mkfs::SNAPSHOT_TREE)),
        "root snapshot",
    );
    let snapshot_tree = v.get(Btree::SnapshotTrees, Bpos::new(0, u64::from(mkfs::SNAPSHOT_TREE), 0)).await?;
    check(
        snapshot_tree.is_some_and(|(k, val)| k.kind == kind::SNAPSHOT_TREE && val == value::snapshot_tree(mkfs::ROOT_SUBVOLUME, snapshot)),
        "snapshot tree",
    );
    let root = v.get(Btree::Inodes, Bpos::new(0, mkfs::ROOT_INODE, snapshot)).await?;
    let root = root.filter(|(k, _)| k.kind == kind::INODE_V3).map(|(_, val)| InodeValue::decode(&val)).transpose()?;
    check(
        root.is_some_and(|r| {
            r.mode() == 0o40755
                && r.size == 0
                && r.field(inode_field::SUBVOL) == u64::from(mkfs::ROOT_SUBVOLUME)
                && r.field(inode_field::DIR) == 0
                && r.field(inode_field::DIR_OFFSET) == 0
                && r.field(inode_field::DEPTH) == 0
        }),
        "root directory",
    );
    let cursor = v.get(Btree::LoggedOps, Bpos::new(1, 1, 0)).await?;
    check(cursor.is_some_and(|(k, _)| k.kind == kind::INODE_ALLOC_CURSOR), "inode cursor");
    Ok(found)
}

const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

/// What one walk of every node collects: the tally, and each btree's keys
/// that a later check compares, in the root subvolume's snapshot.
struct Walk<'a> {
    space: &'a Space,
    tally: Tally<'a>,
    findings: Vec<Finding>,
    /// What every bset and inode stamp is at most.
    clean_seq: u64,
    /// What every non-accounting key's version is at most.
    key_version: u64,
    /// The data checksum type the volume's options imply.
    data_csum: u8,
    alloc: BTreeMap<u64, AllocV4>,
    /// Each freespace key's first and end bucket and generation bits.
    freespace: Vec<(u64, u64, u64)>,
    need_discard: BTreeSet<Bpos>,
    bucket_gens: BTreeMap<Bpos, Vec<u8>>,
    lru: BTreeSet<Bpos>,
    backpointers: BTreeMap<Bpos, Vec<u8>>,
    accounting: BTreeMap<Bpos, Vec<u64>>,
    inodes: BTreeMap<u64, InodeValue>,
    /// Dirents and whiteouts by directory and offset: type and value.
    dirents: BTreeMap<(u64, u64), (u8, Vec<u8>)>,
    /// Extent-btree keys by inode and end: type and size.
    extents: BTreeMap<(u64, u64), (u8, u32)>,
    cursor: Option<u64>,
}

impl<'a> Walk<'a> {
    fn new(space: &'a Space, sb: &Superblock) -> Self {
        let clean = sb.clean.as_ref();
        let data_csum = match (sb.encrypted(), sb.flag(1, 9, 1), sb.flag(0, 44, 4)) {
            (true, 1, _) => 4,
            (true, _, _) => 3,
            (false, _, option) => [0, 5, 6, 7].get(option as usize).copied().unwrap_or(u8::MAX),
        };
        Walk {
            space,
            tally: Tally::new(space),
            findings: Vec::new(),
            clean_seq: clean.map_or(0, |c| c.journal_seq),
            key_version: clean.and_then(|c| c.key_version).unwrap_or(0),
            data_csum,
            alloc: BTreeMap::new(),
            freespace: Vec::new(),
            need_discard: BTreeSet::new(),
            bucket_gens: BTreeMap::new(),
            lru: BTreeSet::new(),
            backpointers: BTreeMap::new(),
            accounting: BTreeMap::new(),
            inodes: BTreeMap::new(),
            dirents: BTreeMap::new(),
            extents: BTreeMap::new(),
            cursor: None,
        }
    }

    /// Every node from every root: inside the bitmap, its children tiling
    /// it, no bset or inode stamped past `clean`'s journal sequence
    /// (upstream's fsck reports an inode above it, and upstream's
    /// read-write mount of such a volume loses it), and every key of a type
    /// its btree holds and by its btree's rules.
    async fn nodes<D: BlockDev>(&mut self, v: &Volume<D>) -> Result<()> {
        let sb = v.superblock();
        let node_sectors = u64::from(sb.node_sectors());
        let mut seen = BTreeSet::new();
        for root in v.roots() {
            let btree = root.btree;
            if root.ptr.min != Bpos::MIN {
                self.findings.push(Finding::Topology { btree, sector: root.ptr.ptr.sector, rule: "root not from the first position" });
            }
            self.tally.node(btree, root.level, &root.ptr.ptr, root.key.pos);
            let mut pending = alloc::vec![(root.level, root.ptr, root.key.pos)];
            while let Some((level, ptr, max)) = pending.pop() {
                let sector = ptr.ptr.sector;
                // Walked once, however many pointers lead to it.
                if !seen.insert(sector) {
                    self.findings.push(Finding::Topology { btree, sector, rule: "node pointed at twice" });
                    continue;
                }
                if !sb.member.bitmap_covers(sector, sector + node_sectors) {
                    self.findings.push(Finding::Bitmap { btree, sector });
                }
                let node = v.node(btree, level, &ptr, max).await?;
                for &seq in node.bset_seqs.iter().filter(|&&s| s > self.clean_seq) {
                    self.findings.push(Finding::BsetAboveClean { btree, sector, seq });
                }
                if level > 0 && node.keys.is_empty() {
                    self.findings.push(Finding::Topology { btree, sector, rule: "interior node with no children" });
                }
                let mut next_min = Some(ptr.min);
                for (key, value) in &node.keys {
                    if !btree.allows(level, key.kind) {
                        self.findings.push(Finding::KeyType { btree, level, kind: key.kind, pos: key.pos });
                        continue;
                    }
                    if level == 0 {
                        self.leaf(btree, key, value)?;
                        continue;
                    }
                    if key.kind != kind::BTREE_PTR_V2 {
                        return Err(super::Error::Unsupported("interior key other than btree_ptr_v2, such as a v1 btree_ptr"));
                    }
                    let child = NodePtr::decode(value)?;
                    if next_min != Some(child.min) {
                        self.findings.push(Finding::Topology { btree, sector, rule: "children not tiling their parent" });
                    }
                    next_min = key.pos.successor();
                    self.tally.node(btree, level - 1, &child.ptr, key.pos);
                    pending.push((level - 1, child, key.pos));
                }
                if level > 0 && node.keys.last().is_some_and(|(k, _)| k.pos != max) {
                    self.findings.push(Finding::Topology { btree, sector, rule: "children not tiling their parent" });
                }
            }
        }
        Ok(())
    }

    fn leaf(&mut self, btree: Btree, key: &Key, value: &[u8]) -> Result<()> {
        self.key_rules(btree, key);
        self.tally.leaf(btree, key, value)?;
        let pos = key.pos;
        let root_snapshot = pos.snapshot == mkfs::ROOT_SNAPSHOT;
        match (btree, key.kind) {
            (Btree::Alloc, kind::ALLOC_V4) if pos.inode == 0 => {
                self.alloc.insert(pos.offset, AllocV4::decode(value)?);
            }
            (Btree::Alloc, kind::ALLOC_V4) => self.findings.push(Finding::Derived { btree, pos, rule: "unexpected" }),
            (Btree::Alloc, _) => return Err(super::Error::Unsupported("alloc key before alloc_v4")),
            (Btree::Freespace, kind::SET) => {
                let end = pos.offset & ((1 << 56) - 1);
                self.freespace.push((end.saturating_sub(u64::from(key.size)), end, pos.offset >> 56));
            }
            (Btree::NeedDiscard, kind::SET) => {
                self.need_discard.insert(pos);
            }
            (Btree::BucketGens, kind::BUCKET_GENS) => {
                self.bucket_gens.insert(pos, value.to_vec());
            }
            (Btree::Lru, kind::SET) => {
                self.lru.insert(pos);
            }
            (Btree::Backpointers, kind::BACKPOINTER) => {
                self.backpointers.insert(pos, value.to_vec());
            }
            (Btree::Accounting, kind::ACCOUNTING) => {
                self.accounting.insert(pos, value.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect());
            }
            (Btree::Inodes, kind::INODE | kind::INODE_V2) => return Err(super::Error::Unsupported("inode in a format before inode_v3")),
            (Btree::Inodes, kind::INODE_V3) if root_snapshot => {
                let inode = InodeValue::decode(value)?;
                if inode.journal_seq > self.clean_seq {
                    self.findings.push(Finding::InodeAboveClean { ino: pos.offset, seq: inode.journal_seq });
                }
                self.inodes.insert(pos.offset, inode);
            }
            (Btree::Dirents, kind::DIRENT | kind::HASH_WHITEOUT | kind::WHITEOUT) if root_snapshot => {
                self.dirents.insert((pos.inode, pos.offset), (key.kind, value.to_vec()));
            }
            (Btree::Extents, kind::WHITEOUT | kind::EXTENT_WHITEOUT) => {}
            (Btree::Extents, _) if root_snapshot => {
                if key.kind == kind::EXTENT {
                    self.extent_csum(key, value)?;
                }
                self.extents.insert((pos.inode, pos.offset), (key.kind, key.size));
            }
            (Btree::LoggedOps, kind::INODE_ALLOC_CURSOR) if pos == Bpos::new(1, 1, 0) && value.len() >= 16 => self.cursor = Some(le64(value, 8)),
            _ => {}
        }
        let snapshots = matches!(btree, Btree::Extents | Btree::Inodes | Btree::Dirents | Btree::Xattrs | Btree::Damage);
        if snapshots && !root_snapshot {
            self.findings.push(Finding::Key { btree, pos, rule: "in a snapshot other than the root subvolume's" });
        }
        Ok(())
    }

    /// A leaf key's size, snapshot field and version, by its btree.
    fn key_rules(&mut self, btree: Btree, key: &Key) {
        let size = if btree.extents() {
            key.kind == kind::WHITEOUT || (key.size != 0 && u64::from(key.size) <= key.pos.offset & ((1 << 56) - 1))
        } else {
            key.size == 0
        };
        let snapshot = match btree {
            Btree::Extents | Btree::Inodes | Btree::Dirents | Btree::Xattrs | Btree::Damage => key.pos.snapshot != 0,
            Btree::DeletedInodes | Btree::Accounting | Btree::ReconcileWork | Btree::ReconcileHipri | Btree::ReconcilePending => true,
            _ => key.pos.snapshot == 0,
        };
        let version = btree == Btree::Accounting || key.version.lo <= self.key_version;
        for (ok, rule) in [
            (size, "size other than its btree's rule"),
            (snapshot, "snapshot field against its btree's rule"),
            (version, "version above the volume's key version"),
        ] {
            if !ok {
                self.findings.push(Finding::Key { btree, pos: key.pos, rule });
            }
        }
    }

    /// An extent's checksum entry: the type the volume's options imply, in
    /// the smallest entry that holds it.
    fn extent_csum(&mut self, key: &Key, value: &[u8]) -> Result<()> {
        let crc = Extent::decode(value)?.crc;
        let pos = key.pos;
        if crc.map_or(0, |c| c.csum_type) != self.data_csum {
            self.findings.push(Finding::Key { btree: Btree::Extents, pos, rule: "data checksum type other than the volume's options imply" });
        }
        if crc.is_some_and(|c| Extent::smallest_entry(&c) != c.csum_len) {
            self.findings.push(Finding::Key { btree: Btree::Extents, pos, rule: "checksum entry other than the smallest that holds it" });
        }
        Ok(())
    }

    /// Dirents against their directories and targets, inodes against their
    /// names and extents, directories against the tree they form.
    fn namespace(&self, root: u64, sb: &Superblock) -> Vec<Finding> {
        let mut found = Vec::new();
        let mut names_of: BTreeMap<u64, u64> = BTreeMap::new();
        let mut subdirs: BTreeMap<u64, u64> = BTreeMap::new();
        // Where the run of consecutive keys holding the current one starts:
        // every slot from a name's hash to its dirent is taken.
        let mut run = 0;
        let mut last: Option<(u64, u64)> = None;
        let mut names: BTreeSet<&[u8]> = BTreeSet::new();
        for (&(dir, offset), (k, v)) in &self.dirents {
            if last != Some((dir, offset.wrapping_sub(1))) {
                run = offset;
            }
            if last.is_none_or(|(d, _)| d != dir) {
                names.clear();
            }
            last = Some((dir, offset));
            if *k != kind::DIRENT {
                continue;
            }
            let mut bad = |rule| found.push(Finding::Dirent { dir, offset, rule });
            let Ok(e) = Dirent::decode(v) else {
                bad("value that does not decode");
                continue;
            };
            let Some(d) = self.inodes.get(&dir).filter(|d| d.mode() & S_IFMT == S_IFDIR) else {
                bad("in no directory");
                continue;
            };
            match hash(d, e.name) {
                Ok(h) if run <= h && h <= offset => {}
                Ok(_) => bad("away from its name's hash slot"),
                Err(_) => bad("in a directory hashed other than by siphash"),
            }
            if !names.insert(e.name) {
                bad("a name twice in one directory");
            }
            if e.target == dir {
                bad("naming its own directory");
            }
            let Some(t) = self.inodes.get(&e.target).filter(|_| e.kind != DirentType::Subvolume) else {
                bad("naming no inode of the root subvolume");
                continue;
            };
            if DirentType::of_mode(t.mode()) != Some(e.kind) {
                bad("of another type than its inode");
            }
            *names_of.entry(e.target).or_default() += 1;
            if e.kind == DirentType::Directory {
                *subdirs.entry(dir).or_default() += 1;
            }
        }

        // Each inode's extents: their sectors, the furthest end, overlaps.
        let mut data: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
        let mut before: Option<(u64, u64)> = None;
        for (&(ino, end), &(k, size)) in &self.extents {
            let pos = Bpos::new(ino, end, mkfs::ROOT_SNAPSHOT);
            let start = end.saturating_sub(u64::from(size));
            if before.is_some_and(|(i, e)| i == ino && start < e) {
                found.push(Finding::Key { btree: Btree::Extents, pos, rule: "overlapping the extent before it" });
            }
            before = Some((ino, end));
            let sums = data.entry(ino).or_default();
            sums.0 += u64::from(size);
            if k != kind::RESERVATION {
                sums.1 = sums.1.max(end);
            }
            match self.inodes.get(&ino).map(|i| i.mode() & S_IFMT) {
                None => found.push(Finding::Key { btree: Btree::Extents, pos, rule: "of no inode" }),
                Some(S_IFREG | S_IFLNK) => {}
                Some(_) => found.push(Finding::Key { btree: Btree::Extents, pos, rule: "of a node that is no file or symlink" }),
            }
        }

        let block = sb.block_bytes() as u64;
        let reachable = self.reachable(root);
        for (&ino, v) in &self.inodes {
            let mut bad = |rule| found.push(Finding::Inode { ino, rule });
            let options = inode_field::OPTIONS.iter().any(|&f| v.field(f) != 0);
            let names = names_of.get(&ino).copied().unwrap_or(0);
            let backref = (v.field(inode_field::DIR), v.field(inode_field::DIR_OFFSET));
            let rules = [
                (ino >= mkfs::ROOT_INODE, "numbered below 4096"),
                (v.flags & inode_flag::HAS_CHILD_SNAPSHOT == 0, "child-snapshot flag"),
                ((v.flags & inode_flag::HAS_INODE_OPTS != 0) == options, "inode-options flag other than its options"),
                (v.flags & inode_flag::ACLS == 0, "ACL flag without its xattr"),
                (v.field(inode_field::PARENT_SUBVOL) == 0, "parent subvolume"),
                ((v.field(inode_field::SUBVOL) != 0) == (ino == root), "subvolume id other than the root directory's"),
                (v.flags & inode_flag::UNLINKED == 0, "unlinked on a clean volume"),
            ];
            for (ok, rule) in rules {
                if !ok {
                    bad(rule);
                }
            }
            if backref != (0, 0) {
                let named = self.dirents.get(&backref).is_some_and(|(k, val)| *k == kind::DIRENT && Dirent::decode(val).is_ok_and(|e| e.target == ino));
                if !named {
                    bad("back-reference to no dirent naming it");
                }
            }
            if v.mode() & S_IFMT == S_IFDIR {
                if v.size != 0 {
                    bad("directory of nonzero size");
                }
                if v.field(inode_field::NLINK) != subdirs.get(&ino).copied().unwrap_or(0) {
                    bad("directory link count other than its subdirectories");
                }
                if ino == root {
                    if names != 0 || v.field(inode_field::DEPTH) != 0 {
                        bad("root directory with a name or a depth");
                    }
                    continue;
                }
                if names != 1 || backref == (0, 0) {
                    bad("directory not named once, by its back-reference");
                }
                let parent_depth = self.inodes.get(&backref.0).map(|p| p.field(inode_field::DEPTH));
                if parent_depth.is_none_or(|d| d.checked_add(1) != Some(v.field(inode_field::DEPTH))) {
                    bad("depth other than its parent's plus one");
                }
                if !reachable.contains(&ino) {
                    bad("directory unreachable from the root");
                }
                continue;
            }
            let nlink = v.field(inode_field::NLINK) + u64::from(v.flags & inode_flag::UNLINKED == 0);
            if nlink != names {
                bad("link count other than its names");
            }
            if names > 0 && backref == (0, 0) {
                bad("named without a back-reference");
            }
            let (sectors, end) = data.get(&ino).copied().unwrap_or((0, 0));
            if v.flags & inode_flag::I_SECTORS_DIRTY == 0 && v.sectors != sectors {
                bad("sectors other than its extents'");
            }
            if end > v.size.div_ceil(block) * (block / SECTOR as u64) {
                bad("extent past its size");
            }
        }
        if let (Some(next), Some(&max)) = (self.cursor, self.inodes.keys().last()) {
            if max >= next && max >= mkfs::FIRST_INODE {
                found.push(Finding::Root("inode cursor at or below an inode number"));
            }
        }
        found
    }

    /// The directories whose back-references lead to `root` without a
    /// loop. Each directory is walked once: a walk stops at a directory
    /// already judged, and judges every one it passed.
    fn reachable(&self, root: u64) -> BTreeSet<u64> {
        let mut judged: BTreeMap<u64, bool> = BTreeMap::from([(root, true)]);
        for (&start, v) in &self.inodes {
            if v.mode() & S_IFMT != S_IFDIR {
                continue;
            }
            let mut path = BTreeSet::new();
            let mut at = start;
            let verdict = loop {
                if let Some(&v) = judged.get(&at) {
                    break v;
                }
                if !path.insert(at) {
                    break false;
                }
                match self.inodes.get(&at).filter(|i| i.mode() & S_IFMT == S_IFDIR) {
                    Some(i) => at = i.field(inode_field::DIR),
                    None => break false,
                }
            };
            judged.extend(path.into_iter().map(|d| (d, verdict)));
        }
        judged.into_iter().filter_map(|(d, ok)| ok.then_some(d)).collect()
    }

    /// The allocator and accounting btrees against what the volume's
    /// pointers and keys derive.
    fn derived(self) -> Vec<Finding> {
        let s = self.space;
        let mut found = self.tally.findings.clone();
        let d = self.tally.finish(&self.alloc);
        let mut put = |btree, pos, rule| found.push(Finding::Derived { btree, pos, rule });
        for b in s.first_bucket..s.nbuckets {
            let stored = self.alloc.get(&b).copied().unwrap_or_default();
            let pos = Bpos::new(0, b, 0);
            let agrees = match d.buckets.get(&b) {
                Some(u) => {
                    if u.gen.is_some_and(|g| g != stored.gen) {
                        put(Btree::Alloc, pos, "pointer generation other than its bucket's");
                    }
                    stored.data_type == u.data_type && u64::from(stored.dirty) == u.dirty
                }
                None => data_type::empty(stored.data_type) && stored.dirty == 0,
            };
            if !agrees || stored.cached != 0 || stored.stripe_refcount != 0 || stored.stripe_sectors != 0 {
                put(Btree::Alloc, pos, "differs");
            }
        }
        for &b in self.alloc.keys().filter(|&&b| b < s.first_bucket || b >= s.nbuckets) {
            put(Btree::Alloc, Bpos::new(0, b, 0), "unexpected");
        }

        // Free ranges, merged where they touch at equal generation bits.
        let mut stored = self.freespace;
        stored.sort_unstable();
        let mut merged: Vec<(u64, u64, u64)> = Vec::new();
        for r in stored {
            match merged.last_mut() {
                Some(last) if last.1 == r.0 && last.2 == r.2 => last.1 = r.1,
                _ => merged.push(r),
            }
        }
        let want: BTreeSet<(u64, u64, u64)> = d.freespace.iter().map(|&(p, size)| {
            let end = p.offset & ((1 << 56) - 1);
            (end - u64::from(size), end, p.offset >> 56)
        }).collect();
        let have: BTreeSet<(u64, u64, u64)> = merged.into_iter().collect();
        let range_pos = |&(_, end, bits): &(u64, u64, u64)| Bpos::new(0, end | bits << 56, 0);
        for r in want.difference(&have) {
            put(Btree::Freespace, range_pos(r), "missing");
        }
        for r in have.difference(&want) {
            put(Btree::Freespace, range_pos(r), "unexpected");
        }

        let sets = [(Btree::NeedDiscard, d.need_discard.iter().copied().collect::<BTreeSet<_>>(), &self.need_discard), (Btree::Lru, d.lru.iter().copied().collect(), &self.lru)];
        for (btree, want, have) in &sets {
            for &pos in want.difference(have) {
                put(*btree, pos, "missing");
            }
            for &pos in have.difference(want) {
                put(*btree, pos, "unexpected");
            }
        }
        for (pos, v) in &self.bucket_gens {
            let want = d.bucket_gens.get(&pos.offset).copied().unwrap_or([0; 256]);
            if pos.inode != 0 || v[..] != want[..] {
                put(Btree::BucketGens, *pos, "differs");
            }
        }
        for &group in d.bucket_gens.keys() {
            if !self.bucket_gens.contains_key(&Bpos::new(0, group, 0)) {
                put(Btree::BucketGens, Bpos::new(0, group, 0), "missing");
            }
        }
        compare(Btree::Backpointers, &d.backpointers.iter().map(|(&p, v)| (p, v.to_vec())).collect(), &self.backpointers, &mut put);
        compare(Btree::Accounting, &d.accounting, &self.accounting, &mut put);
        found
    }
}

/// A derived btree's keys against the volume's, by position and value.
fn compare<V: PartialEq>(btree: Btree, want: &BTreeMap<Bpos, V>, have: &BTreeMap<Bpos, V>, put: &mut impl FnMut(Btree, Bpos, &'static str)) {
    for (pos, v) in want {
        match have.get(pos) {
            None => put(btree, *pos, "missing"),
            Some(h) if h != v => put(btree, *pos, "differs"),
            Some(_) => {}
        }
    }
    for pos in have.keys().filter(|p| !want.contains_key(p)) {
        put(btree, *pos, "unexpected");
    }
}
