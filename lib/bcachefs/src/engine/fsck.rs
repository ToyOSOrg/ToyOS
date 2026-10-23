//! ToyOS's check of a volume ToyOS wrote: every invariant the writer
//! maintains, grown with each stage that adds one. It is built from the
//! same facts as the writer, so it judges ToyOS's consistency with itself;
//! upstream's fsck is the independent judge.

use alloc::vec::Vec;

use toyos_blockdev::{BlockDev, BLOCK};

use super::bkey::Bpos;
use super::mkfs::{self, COMPAT_CLEAN, COMPAT_INITIALIZED, DOWNGRADE, EXTENT_TYPE_U64S, FEATURES};
use super::read::{read_bytes, Volume};
use super::superblock::{field_type, member_flag, Superblock, HEADER};
use super::value::{self, data_type, inode_field, InodeValue, NodePtr, Subvolume, SUBVOLUME_LIVE};
use super::{kind, Btree, Result, SECTOR, VERSION};

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
}

/// Everything wrong with `volume`; empty when nothing is.
pub async fn fsck<D: BlockDev>(volume: &Volume<D>) -> Result<Vec<Finding>> {
    let sb = volume.superblock();
    let mut found = superblock(sb);
    found.extend(copies(volume.dev(), sb).await?);
    found.extend(root_world(volume).await?);
    found.extend(nodes(volume).await?);
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

/// Every node: inside the bitmap, every key of a type its btree holds, and
/// no bset or inode stamped past
/// `clean`'s journal sequence: upstream's fsck reports an inode above it,
/// and upstream's read-write mount of such a volume loses it.
async fn nodes<D: BlockDev>(v: &Volume<D>) -> Result<Vec<Finding>> {
    let sb = v.superblock();
    let clean = sb.clean.as_ref().map_or(0, |c| c.journal_seq);
    let node_sectors = u64::from(sb.node_sectors());
    let mut found = Vec::new();
    for root in v.roots() {
        let mut pending = alloc::vec![(root.level, root.ptr, root.key.pos)];
        while let Some((level, ptr, max)) = pending.pop() {
            let sector = ptr.ptr.sector;
            if !sb.member.bitmap_covers(sector, sector + node_sectors) {
                found.push(Finding::Bitmap { btree: root.btree, sector });
            }
            let node = v.node(root.btree, level, &ptr, max).await?;
            for &seq in node.bset_seqs.iter().filter(|&&s| s > clean) {
                found.push(Finding::BsetAboveClean { btree: root.btree, sector, seq });
            }
            for (key, value) in &node.keys {
                if !root.btree.allows(level, key.kind) {
                    found.push(Finding::KeyType { btree: root.btree, level, kind: key.kind, pos: key.pos });
                    continue;
                }
                if level > 0 {
                    pending.push((level - 1, NodePtr::decode(value)?, key.pos));
                } else if root.btree == Btree::Inodes && key.kind == kind::INODE_V3 {
                    let seq = InodeValue::decode(value)?.journal_seq;
                    if seq > clean {
                        found.push(Finding::InodeAboveClean { ino: key.pos.offset, seq });
                    }
                }
            }
        }
    }
    Ok(found)
}
