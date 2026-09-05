//! Walking a btree: merging a node's bsets into one ordered list of live keys,
//! and descending from a root to the leaves a key range touches.

use alloc::vec::Vec;

use crate::block_io::BlockIO;

use super::bkey::{Bpos, Key, TYPE_DELETED, TYPE_HASH_WHITEOUT, TYPE_WHITEOUT};
use super::node::{clean_roots, BtreePtr, Node, MAX_DEPTH};
use super::sb::Superblock;
use super::UpstreamError;

/// The btrees `BCH_BTREE_IDS()` numbers that a file needs.
pub const BTREE_EXTENTS: u32 = 0;
pub const BTREE_INODES: u32 = 1;
pub const BTREE_DIRENTS: u32 = 2;
pub const BTREE_SUBVOLUMES: u32 = 8;

/// A node's keys, merged: one key per position, the newest bset winning, with
/// the bset it came from kept so an extent can resolve an overlap by it.
///
/// The node is a log — a later bset overrides an earlier one at the same
/// position — so a reader that concatenated the bsets would serve deleted
/// files and stale extents.
pub fn merged(node: &Node) -> Result<Vec<(usize, Key)>, UpstreamError> {
    let mut keys = node.keys()?;
    // Stable by position, then by bset, so the last entry of each position run
    // is the newest one.
    keys.sort_by(|(ga, a), (gb, b)| a.pos.cmp(&b.pos).then(ga.cmp(gb)));

    let mut out: Vec<(usize, Key)> = Vec::with_capacity(keys.len());
    for (generation, key) in keys {
        match out.last_mut() {
            Some((g, prev)) if prev.pos == key.pos => {
                *g = generation;
                *prev = key;
            }
            _ => out.push((generation, key)),
        }
    }
    out.retain(|(_, k)| !is_tombstone(k.kind));
    Ok(out)
}

/// Types that say "nothing is here", whatever an older bset said.
fn is_tombstone(kind: u8) -> bool {
    matches!(kind, TYPE_DELETED | TYPE_WHITEOUT | TYPE_HASH_WHITEOUT)
}

/// A btree, opened at its root.
pub struct Btree<'a> {
    io: &'a dyn BlockIO,
    sb: &'a Superblock,
    id: u32,
    root: BtreePtr,
    level: u8,
}

impl<'a> Btree<'a> {
    /// Open the btree `id` at the root the clean section records.
    pub fn open(io: &'a dyn BlockIO, sb: &'a Superblock, id: u32) -> Result<Self, UpstreamError> {
        let (_, level, root) = clean_roots(sb)?
            .into_iter()
            .find(|(btree, _, _)| *btree == id)
            .ok_or(UpstreamError::Refused("filesystem has no root for a btree a file needs"))?;
        Ok(Self { io, sb, id, root, level })
    }

    /// Hand every live key whose position is in `from..=to` to `visit`, in
    /// order, stopping early when it answers `false`.
    pub fn range(
        &self,
        from: Bpos,
        to: Bpos,
        visit: &mut dyn FnMut(&Node, usize, &Key) -> Result<bool, UpstreamError>,
    ) -> Result<(), UpstreamError> {
        self.descend(&self.root, self.level, from, to, visit).map(|_| ())
    }

    fn descend(
        &self,
        ptr: &BtreePtr,
        level: u8,
        from: Bpos,
        to: Bpos,
        visit: &mut dyn FnMut(&Node, usize, &Key) -> Result<bool, UpstreamError>,
    ) -> Result<bool, UpstreamError> {
        if level >= MAX_DEPTH {
            return Err(UpstreamError::Refused("btree is deeper than the format allows"));
        }
        let node = Node::read(self.io, self.sb, ptr, self.id, level)?;
        let keys = merged(&node)?;

        if level == 0 {
            for (generation, key) in &keys {
                if key.pos < from {
                    continue;
                }
                if key.pos > to {
                    return Ok(false);
                }
                if !visit(&node, *generation, key)? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }

        // An interior key's position is its child's *last* key, so the first
        // child that can hold `from` is the first key at or above it.
        for (_, key) in &keys {
            if key.pos < from {
                continue;
            }
            let child = BtreePtr::read(&node.value(key)?)?;
            if !self.descend(&child, level - 1, from, to, visit)? {
                return Ok(false);
            }
            if key.pos >= to {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The one live key at exactly `pos`, if the btree holds one.
    pub fn get(&self, pos: Bpos) -> Result<Option<(Vec<u8>, Key)>, UpstreamError> {
        let mut found = None;
        self.range(pos, pos, &mut |node, _, key| {
            if key.pos == pos {
                found = Some((node.value(key)?.bytes().to_vec(), *key));
            }
            Ok(false)
        })?;
        Ok(found)
    }
}
