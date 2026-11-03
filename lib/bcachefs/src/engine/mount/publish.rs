//! A clean unmount's last write: every change still held, and every key
//! that writing it derives, in one set of node writes that describes
//! itself. The clean entry's roots alone make it reachable (A1.42), so it
//! leaves nothing to write after it.
//!
//! **Why reclaim alone cannot finish.** Reclaim writes a node and then
//! journals the update that reaches it; a rewrite to a new node also
//! changes allocator, backpointer and accounting keys, which dirty further
//! nodes. Where a node is one block every write is such a rewrite, so the
//! last write always leaves another. A longer node takes appends until its
//! tail is spent, then rewrites again: a tail delays the cascade, it does
//! not bound it.
//!
//! **What this does instead.** The changed nodes are the first members. A
//! member is written once: appended to when its bset fits after its written
//! sectors, otherwise rewritten into new nodes split at fixed bounds. Every
//! key those writes derive is computed from the plan alone, before
//! anything is written, and a node it lands in joins the plan. Nothing is
//! journaled; the keys go into the members' own bytes, whose bsets carry
//! the clean entry's sequence, so a crash before that entry reads none of
//! them (A2.15). New nodes take only slots the durable volume leaves free:
//! a bucket this publication empties is not taken again before then.
//!
//! **Termination.** A plan only grows: members join and stay, an append
//! may become a rewrite but never back, and a rewrite's bounds are only
//! added, each new one splitting an overfull group at one of its keys. All
//! of these are drawn from the volume's finite nodes and positions, so
//! growth stops. Every derived key's position and size depends on the plan
//! alone: a pointer's on its node's bounds, a backpointer's on its slot,
//! an allocator key's on the buckets the plan fills and empties. Only
//! values (written sectors, a node's maximum) follow from sizes. So once
//! the plan stops growing the derivation repeats within two rounds.

use super::reclaim::{mark_bitmap, Pointing};
use super::*;

/// How a member is written.
enum Out {
    /// A bset after its written sectors.
    Append,
    /// New nodes: the maximum of every one but the last, a slot for each,
    /// and a root above them when a root splits.
    Fresh { bounds: Vec<Bpos>, slots: Vec<Slot>, above: Option<Slot> },
}

/// A new node's place and sequence.
#[derive(Clone, Copy)]
struct Slot {
    at: (u64, u8),
    seq: u64,
}

/// What a plan's writes change, in the members, stamped with the clean
/// entry's sequence.
#[derive(Default, PartialEq)]
struct Derived {
    updates: BTreeMap<(Btree, u8, Bpos), journal::Replayed>,
    accounting: BTreeMap<Bpos, Vec<u64>>,
    roots: Vec<Root>,
}

/// Plans settle within this many rounds of the last growth.
const SETTLE: usize = 3;

impl<D: BlockDev> Mount<D> {
    /// Writes every changed node as one self-describing set, the clean
    /// entry `seq` its publication. Nothing is pending, unpointed or
    /// held, and the journal is written through `seq - 1`.
    pub(super) async fn publish(&self, seq: u64) -> Result<()> {
        let mut plan: BTreeMap<NodeId, Out> = BTreeMap::new();
        let changed: Vec<NodeId> = {
            let st = self.st.borrow();
            assert!(st.pending.oldest_pin().is_none() && st.journal.open.is_none() && st.journal.closed.is_empty() && st.journal.next_seq == seq);
            st.cache.ids().into_iter().filter(|&id| {
                let n = st.cache.node(id);
                assert!(n.captured.is_none() && !n.writing && n.held == 0, "a node in flight at the clean unmount");
                !n.unwritten.is_empty()
            }).collect()
        };
        let mut derived = Derived::default();
        for id in changed {
            self.join(&mut plan, id, &derived);
        }
        self.place(&mut plan).await?;
        let mut settled = 0;
        let mut rounds = 0;
        loop {
            rounds += 1;
            let (next, touched, held) = self.derive(&plan, &derived, seq).await?;
            let grew = self.grow(&mut plan, &next, &touched).await;
            release(&mut self.st.borrow_mut(), &held);
            let grew = grew?;
            if !grew && next == derived {
                break;
            }
            settled = if grew { 0 } else { settled + 1 };
            assert!(settled <= SETTLE, "a terminal publication's derived keys unsettled after {rounds} rounds");
            derived = next;
        }
        self.write_members(&plan, &derived, seq).await?;
        let mut st = self.st.borrow_mut();
        #[cfg(test)]
        {
            st.published = super::Published { members: plan.len(), regrouped: st.published.regrouped };
        }
        for root in &derived.roots {
            st.roots.insert(root.btree, *root);
        }
        for id in st.cache.ids() {
            st.cache.change(id, |n| {
                n.unwritten.clear();
                n.compact = None;
            });
        }
        release(&mut st, &plan.keys().copied().collect::<Vec<_>>());
        Ok(())
    }

    /// Adds `id` to the plan, held until the publication ends: appended to
    /// when it may be and its bset fits, else rewritten.
    fn join(&self, plan: &mut BTreeMap<NodeId, Out>, id: NodeId, derived: &Derived) {
        let mut st = self.st.borrow_mut();
        st.cache.change(id, |n| n.held += 1);
        let keys = content(&st, id, derived, Version::default());
        let out = if self.appends(&st, id, &keys, derived) { Out::Append } else { Out::Fresh { bounds: self.bounds(&keys), slots: Vec::new(), above: None } };
        plan.insert(id, out);
    }

    /// Whether `id`, holding `keys` once `derived` applies, takes its
    /// changes as one more bset in the room after its written sectors.
    fn appends(&self, st: &State, id: NodeId, keys: &[(Key, Vec<u8>)], derived: &Derived) -> bool {
        let n = st.cache.node(id);
        let node_bytes = self.sb.node_sectors() as usize * SECTOR;
        let full = keys.iter().map(|(_, v)| key_bytes(v)).sum::<usize>() > node_bytes - FIRST_KEYS;
        n.written > 0 && !n.fresh && !full && usize::from(n.on_disk) * SECTOR + appended_bytes(self.sb.block_bytes(), n, keys, derived) <= node_bytes
    }

    /// Bounds that spread `keys` over new nodes as reclaim splits: at two
    /// thirds, or as full as needed past two nodes.
    fn bounds(&self, keys: &[(Key, Vec<u8>)]) -> Vec<Bpos> {
        let node_bytes = self.sb.node_sectors() as usize * SECTOR;
        let sizes: Vec<usize> = keys.iter().map(|(_, v)| key_bytes(v)).collect();
        let mut groups = mkfs::spread(&sizes, mkfs::capacity(node_bytes) - FIRST_KEYS);
        if groups.len() > 2 {
            groups = mkfs::spread(&sizes, node_bytes - FIRST_KEYS);
        }
        groups[..groups.len() - 1].iter().map(|r| keys[r.end - 1].0.pos).collect()
    }

    /// The keys `plan`'s writes change, computed against the nodes as they
    /// stand, with the members' contents taken from `prev`; the nodes they
    /// land in, and the holds taken on them.
    async fn derive(&self, plan: &BTreeMap<NodeId, Out>, prev: &Derived, seq: u64) -> Result<(Derived, BTreeSet<NodeId>, Vec<NodeId>)> {
        let mut txn = Txn::default();
        let mut fills = Vec::new();
        let mut drains = Vec::new();
        for (&id, out) in plan {
            let (btree, level, root) = {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                (n.btree, n.level, is_root(&st, id))
            };
            match out {
                Out::Append => {
                    let sectors = {
                        let st = self.st.borrow();
                        let n = st.cache.node(id);
                        let keys = content(&st, id, prev, Version::default());
                        n.on_disk + (appended_bytes(self.sb.block_bytes(), n, &keys, prev) / SECTOR) as u16
                    };
                    self.reach(&mut txn, &[id], btree, level, root, Pointing::Append { sectors }).await?;
                }
                Out::Fresh { .. } => {
                    let (by, above) = self.planned(id, out, prev, seq);
                    self.reach(&mut txn, &[id], btree, level, root, Pointing::Fresh { by: &by, above: above.as_ref() }).await?;
                    fills.extend(by.into_iter().chain(above));
                    drains.push(id);
                }
            }
        }
        // A bucket a rewrite fills is not emptied by another's release:
        // its generation stays the one the new pointers carry.
        let mut buckets = BTreeMap::new();
        for p in &fills {
            self.node_counted(&mut txn, &mut buckets, p.btree, p.level, p.sector, p.gen, p.max, true).await?;
        }
        for &id in &drains {
            self.released(&mut txn, &mut buckets, id).await?;
        }
        for (b, (before, after)) in buckets {
            self.bucket_changed(&mut txn, b, before, after).await?;
        }
        let stamped = stamped(&txn, seq);
        let derived = Derived { updates: stamped.updates, accounting: stamped.accounting, roots: stamped.roots };
        Ok((derived, txn.dirtied, txn.held))
    }

    /// Grows `plan` for `derived`: a node it changes joins; an append whose
    /// bset no longer fits becomes a rewrite; a new node past full is
    /// split at more bounds; and every new node gets its slot. Whether it
    /// grew.
    async fn grow(&self, plan: &mut BTreeMap<NodeId, Out>, derived: &Derived, touched: &BTreeSet<NodeId>) -> Result<bool> {
        let mut grew = false;
        for &id in touched {
            if !plan.contains_key(&id) {
                self.join(plan, id, derived);
                grew = true;
            }
        }
        let full = self.group_limit();
        #[cfg(test)]
        let mut regrouped = 0;
        for (&id, out) in plan.iter_mut() {
            let st = self.st.borrow();
            let keys = content(&st, id, derived, Version::default());
            match out {
                Out::Append => {
                    if !self.appends(&st, id, &keys, derived) {
                        *out = Out::Fresh { bounds: self.bounds(&keys), slots: Vec::new(), above: None };
                        grew = true;
                    }
                }
                Out::Fresh { bounds, .. } => {
                    let n = st.cache.node(id);
                    let mut more = Vec::new();
                    for Group { keys, .. } in groups(n.min, n.max, bounds, &keys) {
                        let sizes: Vec<usize> = keys.iter().map(|(_, v)| key_bytes(v)).collect();
                        if sizes.iter().sum::<usize>() > full {
                            let split = mkfs::spread(&sizes, full * 2 / 3);
                            more.extend(split[..split.len() - 1].iter().map(|r| keys[r.end - 1].0.pos));
                        }
                    }
                    if !more.is_empty() {
                        #[cfg(test)]
                        {
                            regrouped += more.len();
                        }
                        bounds.extend(more);
                        bounds.sort_unstable();
                        grew = true;
                    }
                }
            }
        }
        #[cfg(test)]
        {
            self.st.borrow_mut().published.regrouped += regrouped;
        }
        self.place(plan).await?;
        Ok(grew)
    }

    /// Bytes of keys a new node takes: a node's, or less as a test sets.
    fn group_limit(&self) -> usize {
        let full = self.sb.node_sectors() as usize * SECTOR - FIRST_KEYS;
        #[cfg(test)]
        let full = self.st.borrow().group_limit.unwrap_or(full);
        full
    }

    /// A slot for every new node `plan` writes, and for a root above a
    /// root that splits.
    async fn place(&self, plan: &mut BTreeMap<NodeId, Out>) -> Result<()> {
        for (&id, out) in plan.iter_mut() {
            let Out::Fresh { bounds, slots, above } = out else { continue };
            while slots.len() <= bounds.len() {
                slots.push(Slot { at: self.alloc_sector().await?, seq: self.nonzero()? });
            }
            let (level, root) = {
                let st = self.st.borrow();
                (st.cache.node(id).level, is_root(&st, id))
            };
            if root && !bounds.is_empty() && above.is_none() {
                if level == 3 {
                    return Err(Error::Unsupported("a btree deeper than four levels"));
                }
                *above = Some(Slot { at: self.alloc_sector().await?, seq: self.nonzero()? });
            }
        }
        Ok(())
    }

    /// The new nodes a rewrite of `id` writes, holding its keys once `prev`
    /// applies, and the root above them.
    fn planned(&self, id: NodeId, out: &Out, prev: &Derived, seq: u64) -> (Vec<Planned>, Option<Planned>) {
        let Out::Fresh { bounds, slots, above } = out else { unreachable!("an append plans no new node") };
        let st = self.st.borrow();
        let n = st.cache.node(id);
        let block = self.sb.block_bytes();
        let keys = content(&st, id, prev, Version { hi: (seq >> 32) as u32, lo: seq << 32 });
        let sectors = |keys: &[(Key, Vec<u8>)]| (node::record_bytes(keys.iter().map(|(_, v)| key_bytes(v))).next_multiple_of(block) / SECTOR) as u16;
        let by: Vec<Planned> = groups(n.min, n.max, bounds, &keys)
            .into_iter()
            .zip(slots)
            .map(|(Group { min, max, keys }, slot)| Planned { btree: n.btree, level: n.level, min, max, seq: slot.seq, sector: slot.at.0, gen: slot.at.1, sectors: sectors(&keys), journal_seq: seq, keys })
            .collect();
        let above = above.filter(|_| by.len() > 1).map(|slot| {
            let keys: Vec<(Key, Vec<u8>)> = by.iter().map(|p| reclaim::pointer(p.max, &p.ptr())).collect();
            Planned { btree: n.btree, level: n.level + 1, min: Bpos::MIN, max: Bpos::MAX, seq: slot.seq, sector: slot.at.0, gen: slot.at.1, sectors: sectors(&keys), journal_seq: seq, keys }
        });
        (by, above)
    }

    /// Encodes and writes every member as the settled plan has it: the
    /// superblock first when a new node lies outside the btree bitmap
    /// (A1.27), then each node's bytes.
    async fn write_members(&self, plan: &BTreeMap<NodeId, Out>, derived: &Derived, seq: u64) -> Result<()> {
        let magic = node::magic(&self.sb.header.internal_uuid);
        let block = self.sb.block_bytes();
        let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
        for (&id, out) in plan {
            match out {
                Out::Append => {
                    let st = self.st.borrow();
                    let n = st.cache.node(id);
                    let keys = content(&st, id, derived, Version { hi: (seq >> 32) as u32, lo: seq << 32 });
                    let changed: Vec<Vec<u8>> = changed_positions(n, derived)
                        .into_iter()
                        .map(|pos| match keys.binary_search_by(|(k, _)| k.pos.cmp(&pos)) {
                            Ok(i) => keys[i].0.encode(&keys[i].1),
                            Err(_) => Key::new(kind::DELETED, pos).encode(&[]),
                        })
                        .collect();
                    let at = usize::from(n.on_disk) * SECTOR;
                    let later = node::Later { btree: n.btree, level: n.level, seq: n.seq, offset: at, journal_seq: seq, keys: &changed };
                    writes.push((n.sector + u64::from(n.on_disk), node::encode_later(block, self.key.as_ref(), &later)?));
                }
                Out::Fresh { .. } => {
                    let (by, above) = self.planned(id, out, derived, seq);
                    for p in by.into_iter().chain(above) {
                        let (written, bytes) = self.fresh_node(p.btree, p.level, p.min, p.max, seq, p.keys, (p.sector, p.gen), p.seq, magic)?;
                        assert_eq!(written.sectors, p.sectors, "a new node's size moved from its derived pointer");
                        writes.push((p.sector, bytes));
                    }
                }
            }
        }
        let node = u64::from(self.sb.node_sectors());
        let marked = {
            let mut st = self.st.borrow_mut();
            let mut marked = false;
            for out in plan.values() {
                if let Out::Fresh { slots, above, .. } = out {
                    for slot in slots.iter().chain(above) {
                        marked |= mark_bitmap(&mut st.member, slot.at.0, slot.at.0 + node);
                    }
                }
            }
            marked
        };
        if marked {
            self.write_superblock(None).await?;
        }
        for (sector, bytes) in writes {
            self.reissue("btree node write failed", async || mkfs::write(&self.dev, sector, &bytes).await).await?;
            self.st.borrow_mut().clocks[1] += (bytes.len() / SECTOR) as u64;
        }
        Ok(())
    }
}

/// Whether `id` is its btree's root.
fn is_root(st: &State, id: NodeId) -> bool {
    let n = st.cache.node(id);
    st.new_roots.get(&n.btree) == Some(&id) || st.roots.get(&n.btree).is_some_and(|r| r.ptr.ptr.sector == n.sector)
}

/// `id`'s keys once `derived` applies: its replacements and deletions, and
/// an accounting leaf's deltas summed under `version`.
fn content(st: &State, id: NodeId, derived: &Derived, version: Version) -> Vec<(Key, Vec<u8>)> {
    let n = st.cache.node(id);
    let mut keys: BTreeMap<Bpos, (Key, Vec<u8>)> = n.keys.iter().map(|(k, v)| (k.pos, (*k, v.clone()))).collect();
    for (&(_, _, pos), kv) in derived.updates.range((n.btree, n.level, n.min)..=(n.btree, n.level, n.max)) {
        match kv {
            Some((k, v)) => keys.insert(pos, (Key { pos, ..*k }, v.clone())),
            None => keys.remove(&pos),
        };
    }
    if n.btree == Btree::Accounting && n.level == 0 {
        for (&pos, delta) in derived.accounting.range(n.min..=n.max) {
            let (key, value) = keys.entry(pos).or_insert_with(|| (Key::new(kind::ACCOUNTING, pos), Vec::new()));
            let mut sums = if value.is_empty() { Vec::new() } else { journal::counters(value).expect("cached accounting decodes") };
            sums.resize(sums.len().max(delta.len()), 0);
            for (sum, d) in sums.iter_mut().zip(delta) {
                *sum = sum.wrapping_add(*d);
            }
            key.version = version;
            *value = value::counters(&sums);
        }
    }
    keys.into_values().collect()
}

/// The positions an append to `n` carries: its own changes and those
/// `derived` makes in its range.
fn changed_positions(n: &Cached, derived: &Derived) -> BTreeSet<Bpos> {
    let mut positions: BTreeSet<Bpos> = n.unwritten.keys().copied().collect();
    positions.extend(derived.updates.range((n.btree, n.level, n.min)..=(n.btree, n.level, n.max)).map(|(&(_, _, pos), _)| pos));
    if n.btree == Btree::Accounting && n.level == 0 {
        positions.extend(derived.accounting.range(n.min..=n.max).map(|(&pos, _)| pos));
    }
    positions
}

/// Bytes on the medium of the bset an append to `n` writes, `keys` its
/// keys once `derived` applies.
fn appended_bytes(block: usize, n: &Cached, keys: &[(Key, Vec<u8>)], derived: &Derived) -> usize {
    let sizes = changed_positions(n, derived).into_iter().map(|pos| match keys.binary_search_by(|(k, _)| k.pos.cmp(&pos)) {
        Ok(i) => key_bytes(&keys[i].1),
        Err(_) => key_bytes(&[]),
    });
    node::later_record_bytes(sizes).next_multiple_of(block)
}

/// One new node's range and keys.
struct Group {
    min: Bpos,
    max: Bpos,
    keys: Vec<(Key, Vec<u8>)>,
}

/// `keys`, of a node over `[min, max]`, cut at `bounds`.
fn groups(min: Bpos, max: Bpos, bounds: &[Bpos], keys: &[(Key, Vec<u8>)]) -> Vec<Group> {
    let mut out = Vec::with_capacity(bounds.len() + 1);
    let mut low = min;
    let mut rest = keys;
    for &high in bounds.iter().chain(core::iter::once(&max)) {
        let take = rest.partition_point(|(k, _)| k.pos <= high);
        out.push(Group { min: low, max: high, keys: rest[..take].to_vec() });
        rest = &rest[take..];
        low = high.successor().unwrap_or(high);
    }
    out
}
