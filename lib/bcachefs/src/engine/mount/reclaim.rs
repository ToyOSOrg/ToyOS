//! Reclaim: dirty nodes written outside the slot, then made reachable by
//! a transaction on reclaim's own reserves.

use super::*;

pub(super) fn node_ptr(sector: u64, gen: u8) -> Ptr {
    Ptr { sector, dev: 0, gen, cached: false, unwritten: false }
}

/// The key of a pointer to `ptr`, a node whose max key is `max`.
fn pointer(max: Bpos, ptr: &NodePtr) -> (Key, Vec<u8>) {
    (Key::new(kind::BTREE_PTR_V2, max), ptr.encode().to_vec())
}

/// Which dirty nodes reclaim writes first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    /// The oldest pin first, at any level: what frees journal buckets.
    OldestPin,
    /// The lowest level's first: a clean unmount's level-by-level writes.
    LeavesFirst,
}

/// What a reclaim batch writes for one node.
pub(crate) struct Plan {
    ids: Vec<NodeId>,
    btree: Btree,
    level: u8,
    /// It is its btree's root.
    root: bool,
    write: Write,
}

enum Write {
    /// A bset appended at sector `at` of the node.
    Append { at: u64, bytes: Vec<u8> },
    /// New nodes, each with its bytes, and a new root above them when a
    /// root split.
    Fresh { nodes: Vec<(Planned, Vec<u8>)>, root: Option<(Planned, Vec<u8>)> },
}

impl Plan {
    /// New nodes it writes: those holding the keys, and a root above them.
    fn written(&self) -> usize {
        match &self.write {
            Write::Append { .. } => 0,
            Write::Fresh { nodes, root } => nodes.len() + usize::from(root.is_some()),
        }
    }

    /// u64s its pointer update journals at most.
    fn cost(&self) -> u64 {
        match self.written() {
            0 => APPEND_COST,
            k => fresh_cost(k) + (self.ids.len() - 1) as u64 * (ALLOC + 4 * SET + BUCKET_GENS),
        }
    }

    /// Nodes its pointer update dirties at most.
    fn nodes(&self) -> usize {
        match self.written() {
            0 => 1,
            k => fresh_nodes(k) + 7 * (self.ids.len() - 1),
        }
    }
}

/// u64s of a key in a btree_keys record whose value is `bytes` long.
const fn key_u64s(bytes: usize) -> u64 {
    (KEY_U64S + bytes.div_ceil(8)) as u64
}

/// A pointer to a node.
const POINTER: u64 = key_u64s(NodePtr::BYTES);
/// A key of no value: freespace, LRU and need_discard entries, and a
/// deletion.
const SET: u64 = key_u64s(0);
const ALLOC: u64 = key_u64s(Bucket::BYTES);
const BUCKET_GENS: u64 = key_u64s(value::BUCKET_GENS_BYTES);
const BACKPOINTER: u64 = key_u64s(value::BACKPOINTER_BYTES);
const COUNTERS: u64 = key_u64s(8 * value::MAX_COUNTERS);
/// Accounting a rewrite changes: its btree's class, the btree replicas
/// entry, and the device's free, btree and need_discard states (A7.12).
const ACCOUNTED: u64 = 5;
/// Record headers, one for each btree and level a rewrite changes: the
/// parent's, and alloc, freespace, LRU, need_discard, bucket_gens,
/// backpointers and accounting.
const HEADERS: u64 = 8;

/// An append's update: the pointer's written sectors, in its record.
const APPEND_COST: u64 = 1 + POINTER;

/// u64s the pointer update of a node rewritten to `nodes` new ones
/// journals at most. Each new node: its pointer, its bucket's change and its
/// backpointer. A bucket that takes a node changes most when taken from
/// free: its alloc key, the freespace range split about it and an LRU entry
/// set; one taken from need_discard changes its alloc key, its need_discard
/// entry and an LRU entry; one that already holds nodes, its alloc key and
/// LRU entry.
/// The old node: its bucket's change, most when it empties (alloc key, its
/// LRU entry cleared, need_discard and its bucket_gens group), and its
/// backpointer deleted. And the accounting keys.
pub(super) const fn fresh_cost(nodes: usize) -> u64 {
    let taken = ALLOC + 2 * SET + SET;
    let emptied = ALLOC + SET + SET + BUCKET_GENS;
    HEADERS + nodes as u64 * (POINTER + taken + BACKPOINTER) + emptied + SET + ACCOUNTED * COUNTERS
}

/// Nodes the pointer update of a node rewritten to `nodes` new ones
/// dirties at most: a leaf for each position [`fresh_cost`] changes. The
/// parent; each new node's bucket's alloc, two freespace (or one
/// need_discard) and one LRU position, and its backpointer; the old node's
/// bucket's alloc, LRU, two need_discard (its sequence is not yet known)
/// and bucket_gens positions, and its backpointer; and the accounting.
pub(super) const fn fresh_nodes(nodes: usize) -> usize {
    1 + 5 * nodes + 5 + 1 + ACCOUNTED as usize
}

/// Nodes a split takes at most. A node this writer wrote holds at most a
/// node's keys; one read from the medium may hold five times that once
/// unpacked, a packed key part being one u64 or more and an unpacked one
/// five (A3.15-A3.17). Either takes at most two transactions more before
/// reclaim, which takes an oversized node first, plans it: the one holding
/// the slot, as no transaction enters it while a changed node is oversized
/// ([`Mount::transact`]), and the pointer update of the one reclaim round in
/// flight, each at most `K_TX` u64s of keys. Spread evenly, all that fills
/// this many nodes of at least [`MIN_NODE`], each a key short of full. A
/// replay installs its window's keys with no such bound
/// (`issues/a-replayed-bcachefs-node-past-seven-nodes-is-refused.md`).
pub(super) const MAX_GROUPS: usize = 7;

/// New nodes one rewrite writes at most: a split's, and a root above them.
pub(super) const MAX_REWRITE: usize = MAX_GROUPS + 1;

/// New nodes a rewrite that is not of an oversized node writes at most: a
/// node no fuller than it holds spreads at two thirds into two, and a root.
/// So does one this writer filled past full: it holds at most a node's keys
/// and two transactions', which fit in two nodes.
pub(super) const COMMON_REWRITE: usize = 3;

/// Node slots one batch can take: its common rewrites, or one alone.
const BATCH_SLOTS: u64 = (if BATCH * COMMON_REWRITE > MAX_REWRITE { BATCH * COMMON_REWRITE } else { MAX_REWRITE }) as u64;

/// Cache room reclaim's transaction takes past the limit, for the leaves
/// it dirties: every position of a rewrite to one node in a leaf of its
/// own. A batch whose positions could need more is granted this, as they
/// share leaves; one that dirties more is refused and stops the mount, never
/// overfilling the cache.
pub(super) const RECLAIM_ROOM: usize = fresh_nodes(1);

/// Nodes past the limit a batch's rewrites can leave dirty: their new
/// nodes, each taking the changes since its node was captured, beside the
/// node each replaces. A batch of common rewrites, or one alone.
pub(super) const REWRITTEN: usize = {
    let common = BATCH * (COMMON_REWRITE - 1);
    if common > MAX_REWRITE - 1 { common } else { MAX_REWRITE - 1 }
};

const _: () = assert!(fresh_cost(MAX_REWRITE) <= K_TX, "the largest rewrite fits one reclaim transaction");

impl<D: BlockDev> Mount<D> {
    /// One batch of dirty nodes chosen by `order`, planned under the slot
    /// and written outside it; then their pointer updates journaled. The
    /// journal is written once that frees a bucket of its ring, so a sweep's
    /// updates share entries. Whether there was any node to write.
    pub(super) async fn reclaim_round(&self, order: Order) -> Result<bool> {
        let plans = self.write_batch(order).await?;
        if plans.is_empty() {
            return Ok(false);
        }
        self.point_batch(plans).await?;
        if self.st.borrow().journal_frees() {
            self.flush_open().await?;
        }
        Ok(true)
    }

    /// Reclaim's first half: a batch planned under the slot and written.
    /// Its nodes take changes again once written; their pins hold until
    /// [`Mount::point_batch`].
    pub(crate) async fn write_batch(&self, order: Order) -> Result<Vec<Plan>> {
        let out = self.write_batch_once(order).await;
        self.stopped_by(out)
    }

    async fn write_batch_once(&self, order: Order) -> Result<Vec<Plan>> {
        while self.collapse_root().await? {}
        self.make_room().await?;
        let plans = {
            let _slot = self.slot(Class::Reclaim).await;
            self.plan(order, None).await?
        };
        if plans.is_empty() {
            return Ok(plans);
        }
        self.write_plans(&plans).await?;
        Ok(plans)
    }

    /// Moves the reachable node still at `expected_sector`. Bucket
    /// evacuation uses the same publication and reader barriers as reclaim.
    pub(crate) async fn relocate_node(&self, btree: Btree, level: u8, pos: Bpos, expected_sector: u64) -> Result<bool> {
        let out = async {
            while self.collapse_root().await? {}
            self.make_room().await?;
            let plans = {
                let _slot = self.slot(Class::Reclaim).await;
                if self.st.borrow().roots.get(&btree).is_none_or(|r| r.level < level) { return Ok(false); }
                let id = self.find(btree, level, pos).await?;
                {
                    let mut st = self.st.borrow_mut();
                    let seq = st.journal.open.as_ref().map_or(st.journal.next_seq, |o| o.seq);
                    let n = st.cache.node(id);
                    if n.sector != expected_sector || n.writing || n.captured.is_some() || n.held != 0 { return Ok(false); }
                    st.cache.change(id, |n| { n.fresh = true; n.compact = Some(seq); });
                }
                self.plan(Order::OldestPin, Some(id)).await?
            };
            if plans.is_empty() { return Ok(false); }
            self.write_plans(&plans).await?;
            self.point_batch(plans).await?;
            Ok(true)
        }.await;
        self.stopped_by(out)
    }

    async fn collapse_root(&self) -> Result<bool> {
        let candidate = {
            let st = self.st.borrow();
            st.roots.values().find_map(|r| {
                let id = st.cache.id_at(r.ptr.ptr.sector)?;
                let n = st.cache.node(id);
                (n.level > 0 && n.keys.len() == 1 && n.dirty() && !n.writing && n.captured.is_none() && n.held == 0).then_some((r.btree, n.sector))
            })
        };
        let Some((btree, expected)) = candidate else { return Ok(false) };
        let bound = fresh_cost(1);
        let room = RECLAIM_ROOM;
        self.flush_closed().await?;
        self.admit(Class::Reclaim, Draw::Freeing, bound, room).await?;
        let _slot = self.slot(Class::Reclaim).await;
        let mut txn = Txn::default();
        let out = async {
            let id = {
                let st = self.st.borrow();
                if st.roots.get(&btree).is_none_or(|r| r.ptr.ptr.sector != expected) { return Ok(false); }
                let Some(id) = st.cache.id_at(expected) else { return Ok(false) };
                id
            };
            let (level, sector, gen, key, ptr) = {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                if n.keys.len() != 1 || n.writing || n.captured.is_some() || n.held != 0 { return Ok(false); }
                let (key, value) = &n.keys[0];
                (n.level, n.sector, n.gen, *key, NodePtr::decode(value)?)
            };
            let child = self.load(btree, level - 1, &ptr, key.pos).await?;
            {
                let st = self.st.borrow();
                let n = st.cache.node(child);
                if n.writing || n.captured.is_some() { return Ok(false); }
            }
            if ptr.min != Bpos::MIN || key.pos != Bpos::MAX { return Err(Error::Corrupt("only root child does not span the btree")); }
            txn.roots.push(Root { btree, level: level - 1, key, ptr });
            let mut buckets = BTreeMap::new();
            self.node_counted(&mut txn, &mut buckets, btree, level, sector, gen, Bpos::MAX, false).await?;
            for (b, (before, after)) in buckets { self.bucket_changed(&mut txn, b, before, after).await?; }
            txn.installs.push(Install::Collapse { id });
            Ok(true)
        }.await;
        self.finish(txn, out.is_ok(), Class::Reclaim, bound, room)?;
        out
    }

    /// Reclaim's second half: the written batch's pointer updates
    /// journaled, on reclaim's reserves and ahead in the slot.
    pub(crate) async fn point_batch(&self, plans: Vec<Plan>) -> Result<()> {
        let out = self.point_batch_once(plans).await;
        self.stopped_by(out)
    }

    /// `out`, which a reclaim half returned; a failure stops the mount,
    /// read-only by the failure's name: what the half captured or wrote can
    /// no longer be made durable.
    fn stopped_by<T>(&self, out: Result<T>) -> Result<T> {
        if let Err(e) = &out {
            self.stop(match *e {
                Error::NoSpace(why) | Error::Corrupt(why) | Error::Unsupported(why) | Error::ReadOnly(why) | Error::Checksum(why) | Error::Invalid(why) => why,
                _ => "a reclaim write failed",
            });
        }
        out
    }

    async fn point_batch_once(&self, plans: Vec<Plan>) -> Result<()> {
        let bound = plans.iter().map(Plan::cost).sum();
        let nodes = plans.iter().map(Plan::nodes).sum::<usize>().min(RECLAIM_ROOM);
        // Closed entries hold pipeline depth that only writing them returns:
        // reclaim never waits on what it would itself have to do.
        self.flush_closed().await?;
        // Reclaim's room for nodes is its own share; no draw limits it.
        self.admit(Class::Reclaim, Draw::Freeing, bound, nodes).await?;
        let _slot = self.slot(Class::Reclaim).await;
        let mut txn = Txn::default();
        let out = self.point(&mut txn, plans).await;
        self.finish(txn, out.is_ok(), Class::Reclaim, bound, nodes)?;
        out
    }

    /// The batch to write, each node captured: its changes now pinned as
    /// written-but-unpointed, and no change taken until its write completes.
    /// A node is appended to when it may be and has room; otherwise it is
    /// rewritten to fresh nodes, split past two thirds full.
    async fn plan(&self, order: Order, only: Option<NodeId>) -> Result<Vec<Plan>> {
        let chosen: Vec<NodeId> = if let Some(id) = only { alloc::vec![id] } else {
            let st = self.st.borrow();
            // An oversized node first; then by `order`.
            let mut dirty: Vec<(bool, u8, u64, NodeId)> = st
                .cache
                .ids()
                .into_iter()
                .filter_map(|id| {
                    let n = st.cache.node(id);
                    let ready = n.dirty() && !n.writing && n.captured.is_none() && n.held == 0;
                    let level = match order {
                        Order::OldestPin => 0,
                        Order::LeavesFirst => n.level,
                    };
                    ready.then(|| (!st.cache.oversized(n), level, n.pin().expect("a dirty node pins"), id))
                })
                .collect();
            dirty.sort_unstable();
            dirty.into_iter().map(|d| d.3).collect()
        };
        let node_bytes = self.sb.node_sectors() as usize * SECTOR;
        let block = self.sb.block_bytes();
        let magic = node::magic(&self.sb.header.internal_uuid);
        let mut plans = Vec::new();
        let mut spent = 0;
        for id in chosen {
            if plans.len() == BATCH { break; }
            if self.st.borrow().cache.node(id).captured.is_some() { continue; }
            let ids = self.rebalance_pair(id).await?;
            if ids.is_empty() { continue; }
            let (btree, level, root, min, max, keys, changes, append, groups) = {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                let root = st.new_roots.get(&n.btree) == Some(&id) || st.roots.get(&n.btree).is_some_and(|r| r.ptr.ptr.sector == n.sector);
                let later = node::later_record_bytes(n.unwritten.keys().map(|pos| changed(n, *pos).len())).next_multiple_of(block);
                // An oversized node is split: an append would leave it so.
                let append = ids.len() == 1 && n.compact.is_none() && !n.fresh && n.written > 0 && !st.cache.oversized(n) && usize::from(n.on_disk) * SECTOR + later <= node_bytes;
                let keys: Vec<_> = ids.iter().flat_map(|&id| st.cache.node(id).keys.iter().cloned()).collect();
                let changes: BTreeMap<_, _> = ids.iter().flat_map(|&id| st.cache.node(id).unwritten.iter().map(|(&p, &s)| (p, s))).collect();
                let sizes: Vec<usize> = keys.iter().map(|(_, v)| key_bytes(v)).collect();
                // At two thirds, as upstream splits; an oversized node, too
                // full for two at that, in as few as each a node holds.
                let mut groups = mkfs::spread(&sizes, mkfs::capacity(node_bytes) - FIRST_KEYS);
                if groups.len() > 2 {
                    groups = mkfs::spread(&sizes, node_bytes - FIRST_KEYS);
                }
                (n.btree, n.level, root, st.cache.node(ids[0]).min, st.cache.node(*ids.last().unwrap()).max, keys, changes, append, groups)
            };
            if groups.len() > MAX_GROUPS {
                // More than a node read holds once unpacked, and two
                // transactions ([`MAX_GROUPS`]): a replay's leaf.
                return Err(Error::Unsupported("a btree node whose keys fill more than seven nodes"));
            }
            let written = groups.len() + usize::from(groups.len() > 1 && root);
            let cost = if append { APPEND_COST } else { fresh_cost(written) + (ids.len() - 1) as u64 * (ALLOC + 4 * SET + BUCKET_GENS) };
            // A rewrite past the common one is a batch of its own, so the
            // nodes a batch's rewrites leave stay within reclaim's room.
            let alone = !append && written > COMMON_REWRITE;
            if spent + cost > K_TX || (alone && !plans.is_empty()) {
                for &other in &ids { if other != id { self.st.borrow_mut().cache.change(other, |n| n.held -= 1); } }
                break;
            }
            spent += cost;
            let write = if append {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                let keys: Vec<Vec<u8>> = n.unwritten.keys().map(|pos| changed(n, *pos)).collect();
                let journal_seq = n.unwritten.values().copied().max().expect("a dirty node has changes");
                let at = u64::from(n.on_disk);
                let later = node::Later { btree, level, seq: n.seq, offset: at as usize * SECTOR, journal_seq, keys: &keys };
                let bytes = node::encode_later(block, self.key.as_ref(), &later)?;
                Write::Append { at, bytes }
            } else {
                let splits = groups.len() > 1;
                if splits && root && level == 3 {
                    return Err(Error::Unsupported("a btree deeper than four levels"));
                }
                let mut sectors = Vec::new();
                for _ in 0..groups.len() + usize::from(splits && root) {
                    sectors.push(self.alloc_sector().await?);
                }
                let mut nodes = Vec::new();
                for (j, range) in groups.iter().enumerate() {
                    let low = if j == 0 { min } else { keys[range.start - 1].0.pos.successor().expect("a key below the last position") };
                    let high = if j + 1 == groups.len() { max } else { keys[range.end - 1].0.pos };
                    let mine = keys[range.clone()].to_vec();
                    let journal_seq = changes.range(low..=high).map(|(_, s)| *s).max().unwrap_or(0);
                    nodes.push(self.fresh_node(btree, level, low, high, journal_seq, mine, sectors[j], magic)?);
                }
                let above = (splits && root).then(|| {
                    let keys = nodes.iter().map(|(p, _): &(Planned, Vec<u8>)| pointer(p.max, &p.ptr())).collect();
                    self.fresh_node(btree, level + 1, Bpos::MIN, Bpos::MAX, 0, keys, sectors[groups.len()], magic)
                });
                Write::Fresh { nodes, root: above.transpose()? }
            };
            {
                let mut st = self.st.borrow_mut();
                let seq = st.journal.open.as_ref().map_or(st.journal.next_seq, |o| o.seq);
                for &other in &ids {
                    st.cache.change(other, |n| {
                        if other != id { n.held -= 1; }
                        n.captured = Some(n.pin().unwrap_or(seq));
                        n.unwritten.clear();
                        n.compact = None;
                        n.writing = true;
                    });
                }
            }
            plans.push(Plan { ids, btree, level, root, write });
            if alone {
                break;
            }
        }
        Ok(plans)
    }

    pub(super) fn minimum_fill(&self) -> usize {
        (mkfs::capacity(self.sb.node_sectors() as usize * SECTOR) - FIRST_KEYS) / 2 - 255 * 8
    }

    /// An underfull node and one adjacent child of its parent. A busy
    /// sibling leaves the node dirty until that rewrite has published.
    async fn rebalance_pair(&self, id: NodeId) -> Result<Vec<NodeId>> {
        let (btree, level, max) = {
            let st = self.st.borrow();
            let n = st.cache.node(id);
            let root = st.new_roots.get(&n.btree) == Some(&id) || st.roots.get(&n.btree).is_some_and(|r| r.ptr.ptr.sector == n.sector);
            if root {
                return Ok(if n.level > 0 && n.keys.len() == 1 { Vec::new() } else { alloc::vec![id] });
            }
            if n.bytes >= self.minimum_fill() { return Ok(alloc::vec![id]); }
            (n.btree, n.level, n.max)
        };
        let parent = self.find(btree, level + 1, max).await?;
        let adjacent = {
            let st = self.st.borrow();
            let p = st.cache.node(parent);
            let at = p.at(max);
            if p.keys.len() == 1 { return Ok(Vec::new()); }
            let index = if at > 0 { at - 1 } else { at + 1 };
            let (k, v) = p.keys.get(index).ok_or(Error::Corrupt("underfull node absent from its parent"))?;
            (k.pos, NodePtr::decode(v)?)
        };
        let other = self.load(btree, level, &adjacent.1, adjacent.0).await?;
        let mut st = self.st.borrow_mut();
        let n = st.cache.node(other);
        if n.writing || n.captured.is_some() || n.held > 0 || st.cache.oversized(n) { return Ok(Vec::new()); }
        st.cache.change(other, |n| n.held += 1);
        Ok(if adjacent.0 < max { alloc::vec![other, id] } else { alloc::vec![id, other] })
    }

    /// A new node of `keys`, encoded as its one first record.
    #[allow(clippy::too_many_arguments)]
    fn fresh_node(&self, btree: Btree, level: u8, min: Bpos, max: Bpos, journal_seq: u64, keys: Vec<(Key, Vec<u8>)>, at: (u64, u8), magic: u64) -> Result<(Planned, Vec<u8>)> {
        let seq = self.nonzero()?;
        let encoded: Vec<Vec<u8>> = keys.iter().map(|(k, v)| k.encode(v)).collect();
        let build = Build { btree, level, seq, min, max, journal_seq, keys: &encoded };
        let bytes = node::encode(magic, self.sb.block_bytes(), self.key.as_ref(), &build)?;
        let sectors = (bytes.len() / SECTOR) as u16;
        Ok((Planned { btree, level, min, max, seq, sector: at.0, gen: at.1, sectors, keys }, bytes))
    }

    /// Room for a batch's new nodes now, outside the slot: buckets emptied
    /// made durable, and traversals that may still lead into them waited
    /// out. A volume short past that plans anyway; admission keeps room for
    /// reclaim, so a plan that finds no slot is a bug, and stops the mount.
    async fn make_room(&self) -> Result<()> {
        loop {
            let blocked = self.readers.borrow_mut().blocked();
            let (ready, returning) = {
                let st = self.st.borrow();
                (st.alloc.ready(blocked), st.alloc.returning())
            };
            if ready >= BATCH_SLOTS {
                return Ok(());
            }
            if returning {
                self.commit().await?;
            } else if blocked > 0 {
                self.written.until(|| self.readers.borrow_mut().blocked() < blocked || self.st.borrow().read_only.is_some()).await;
                self.check_writable()?;
            } else {
                return Ok(());
            }
        }
    }

    /// The next node slot, and its bucket's generation: the open bucket's,
    /// or the first of a bucket taken.
    async fn alloc_sector(&self) -> Result<(u64, u8)> {
        let bucket = u64::from(self.sb.member.bucket_sectors);
        let node = u64::from(self.sb.node_sectors());
        loop {
            if let Some(slot) = self.st.borrow_mut().alloc.next_slot(bucket, node) {
                return Ok(slot);
            }
            self.take_bucket(false).await?;
        }
    }

    /// Opens an empty bucket for nodes: the oldest whose emptying is durable
    /// and that no traversal may lead into (A6.14, A6.19), else the lowest
    /// free one. Its alloc key must agree it is empty, and no backpointer
    /// lead into it; the nodes cached there are unreachable, and go.
    pub(super) async fn take_bucket(&self, data: bool) -> Result<()> {
        let durable = self.st.borrow().journal.durable;
        let mut emptied = None;
        Source::walk(self, Btree::NeedDiscard, Bpos::MIN, |k, _| {
            if k.pos.inode > durable {
                return Ok(false);
            }
            // The offset is a device and bucket (A6.2); one device.
            if k.pos.offset >> 48 != 0 {
                return Err(Error::Corrupt("a need_discard key of another device"));
            }
            let b = k.pos.offset;
            if k.kind == kind::SET && !self.st.borrow().alloc.is_taken(b) && !self.readers.borrow_mut().blocks(b) {
                emptied = Some((b, k.pos.inode));
            }
            Ok(emptied.is_none())
        })
        .await?;
        let b = match emptied {
            Some((b, _)) => b,
            None => {
                let mut free = None;
                Source::walk(self, Btree::Freespace, Bpos::MIN, |k, _| {
                    let end = k.pos.offset & ((1 << 56) - 1);
                    let st = self.st.borrow();
                    free = (end.saturating_sub(u64::from(k.size))..end).find(|&b| !st.alloc.is_taken(b));
                    Ok(free.is_none())
                })
                .await?;
                free.ok_or(Error::NoSpace("no free bucket for a btree node"))?
            }
        };
        // The indexes are derived; the bucket's alloc key, its backpointers
        // and the nodes the mount holds must agree it is empty.
        let bucket = match Source::get(self, Btree::Alloc, Bpos::new(0, b, 0)).await? {
            Some((k, v)) if k.kind == kind::ALLOC_V4 => {
                let a = AllocV4::decode(&v)?;
                Bucket::of(&a, a.flags, a.io_time_write, a.journal_seq_nonempty)
            }
            Some(_) => return Err(Error::Unsupported("alloc key before alloc_v4")),
            None => Bucket::HOLE,
        };
        let agrees = match emptied {
            Some((_, seq)) => bucket.data_type == data_type::NEED_DISCARD && bucket.journal_seq_empty == seq,
            None => bucket.data_type == data_type::FREE,
        };
        if !agrees || bucket.dirty != 0 {
            return Err(Error::Corrupt("a bucket an index lists as empty whose alloc key holds otherwise"));
        }
        let sectors = u64::from(self.sb.member.bucket_sectors);
        let (start, end) = ((b * sectors) << self.bp_shift(), ((b + 1) * sectors) << self.bp_shift());
        let mut pointed = false;
        Source::walk(self, Btree::Backpointers, Bpos::new(0, start, 0), |k, _| {
            pointed = k.pos.inode == 0 && k.pos.offset < end;
            Ok(false)
        })
        .await?;
        if pointed {
            return Err(Error::Corrupt("a bucket an index lists as empty that a backpointer leads into"));
        }
        let mut st = self.st.borrow_mut();
        if !st.cache.drop_unreachable(b * sectors, (b + 1) * sectors) {
            return Err(Error::Corrupt("a bucket an index lists as empty holding a changed btree node"));
        }
        if data {
            st.alloc.take_data(b, bucket.reused_gen());
        } else {
            st.alloc.take(b, bucket.reused_gen());
        }
        #[cfg(test)]
        {
            st.alloc.reused += usize::from(emptied.is_some());
        }
        Ok(())
    }

    /// Low bits of a backpointer's offset below its sector (A6.25).
    pub(super) fn bp_shift(&self) -> u32 {
        match self.sb.flag(6, 40, 8) {
            0 => 10,
            s => s as u32,
        }
    }

    /// Writes the batch: first the superblock, when a new node lies outside
    /// the btree bitmap (A1.27), then every node, each reissued byte for
    /// byte on failure.
    async fn write_plans(&self, plans: &[Plan]) -> Result<()> {
        let node = u64::from(self.sb.node_sectors());
        let marked = {
            let mut st = self.st.borrow_mut();
            let mut marked = false;
            for p in plans {
                if let Write::Fresh { nodes, root } = &p.write {
                    for (planned, _) in nodes.iter().chain(root) {
                        marked |= mark_bitmap(&mut st.member, planned.sector, planned.sector + node);
                    }
                }
            }
            marked
        };
        if marked {
            self.write_superblock(None).await?;
        }
        for p in plans {
            let writes: Vec<(u64, &[u8])> = match &p.write {
                Write::Append { at, bytes } => alloc::vec![(self.st.borrow().cache.node(p.ids[0]).sector + at, &bytes[..])],
                Write::Fresh { nodes, root } => nodes.iter().chain(root).map(|(planned, bytes)| (planned.sector, &bytes[..])).collect(),
            };
            for (sector, bytes) in writes {
                self.reissue("btree node write failed", async || mkfs::write(&self.dev, sector, bytes).await).await?;
                self.st.borrow_mut().clocks[1] += (bytes.len() / SECTOR) as u64;
            }
        }
        {
            let mut st = self.st.borrow_mut();
            for p in plans {
                for &id in &p.ids { st.cache.change(id, |n| n.writing = false); }
            }
        }
        self.written.wake();
        Ok(())
    }

    /// The batch's pointer updates, with the allocator and accounting keys
    /// of every node it added or released (A6, A7).
    async fn point(&self, txn: &mut Txn, plans: Vec<Plan>) -> Result<()> {
        let mut buckets: BTreeMap<u64, (Bucket, Bucket)> = BTreeMap::new();
        for plan in plans {
            let (min, max, seq, sector, gen) = {
                let st = self.st.borrow();
                let n = st.cache.node(plan.ids[0]);
                (n.min, n.max, n.seq, n.sector, n.gen)
            };
            let (btree, level) = (plan.btree, plan.level);
            match plan.write {
                Write::Append { at, bytes } => {
                    let sectors = (at + (bytes.len() / SECTOR) as u64) as u16;
                    let ptr = NodePtr { seq, written: sectors, min, ptr: node_ptr(sector, gen) };
                    if plan.root {
                        txn.roots.push(Root { btree, level, key: Key::new(kind::BTREE_PTR_V2, Bpos::MAX), ptr });
                    } else {
                        let (k, v) = pointer(max, &ptr);
                        self.put(txn, btree, level + 1, max, Some((k, v))).await?;
                    }
                    txn.installs.push(Install::Append { id: plan.ids[0], sectors });
                }
                Write::Fresh { nodes, root } => {
                    let by: Vec<Planned> = nodes.into_iter().map(|(p, _)| p).collect();
                    let root = root.map(|(p, _)| p);
                    match (&root, plan.root) {
                        (Some(r), _) => txn.roots.push(Root { btree, level: r.level, key: Key::new(kind::BTREE_PTR_V2, Bpos::MAX), ptr: r.ptr() }),
                        (None, true) => txn.roots.push(Root { btree, level, key: Key::new(kind::BTREE_PTR_V2, Bpos::MAX), ptr: by[0].ptr() }),
                        (None, false) => {
                            for &id in &plan.ids {
                                let old_max = self.st.borrow().cache.node(id).max;
                                self.put(txn, btree, level + 1, old_max, None).await?;
                            }
                            for p in &by {
                                self.put(txn, btree, level + 1, p.max, Some(pointer(p.max, &p.ptr()))).await?;
                            }
                        }
                    }
                    for p in by.iter().chain(&root) {
                        self.node_counted(txn, &mut buckets, btree, p.level, p.sector, p.gen, p.max, true).await?;
                    }
                    for &id in &plan.ids {
                        let (sector, gen, max, written) = {
                            let st = self.st.borrow();
                            let n = st.cache.node(id);
                            (n.sector, n.gen, n.max, n.written)
                        };
                        if written > 0 {
                            self.node_counted(txn, &mut buckets, btree, level, sector, gen, max, false).await?;
                        }
                    }
                    txn.installs.push(Install::Replace { ids: plan.ids, by, root });
                }
            }
        }
        for (b, (before, after)) in buckets {
            self.bucket_changed(txn, b, before, after).await?;
        }
        Ok(())
    }

    /// A btree node at `sector` added to its bucket, or released from it:
    /// its bucket's change, its backpointer and its accounting.
    #[allow(clippy::too_many_arguments)]
    async fn node_counted(&self, txn: &mut Txn, buckets: &mut BTreeMap<u64, (Bucket, Bucket)>, btree: Btree, level: u8, sector: u64, gen: u8, max: Bpos, added: bool) -> Result<()> {
        let bucket_sectors = u64::from(self.sb.member.bucket_sectors);
        let node = self.sb.node_sectors();
        let b = sector / bucket_sectors;
        if let alloc::collections::btree_map::Entry::Vacant(slot) = buckets.entry(b) {
            let before = self.bucket(txn, b).await?;
            slot.insert((before, before));
        }
        let after = &mut buckets.get_mut(&b).expect("just inserted").1;
        if added {
            let clock = self.st.borrow().clocks[1];
            after.fill(node, clock, bucket_sectors as u32)?;
            txn.filled.push(b);
        } else {
            after.drain(node)?;
        }
        let pos = Bpos::new(0, sector << self.bp_shift(), 0);
        let bp = added.then(|| (Key::new(kind::BACKPOINTER, pos), value::backpointer(btree as u8, level + 1, data_type::BTREE, gen, node, max).to_vec()));
        self.put(txn, Btree::Backpointers, 0, pos, bp).await?;
        let sign = |c: u64| if added { c } else { c.wrapping_neg() };
        let mut record = alloc::vec![class::BTREE];
        record.extend_from_slice(&(btree as u32).to_le_bytes());
        self.account(txn, &record, &[sign(u64::from(node)), sign(1), sign(u64::from(level > 0))]).await?;
        self.account(txn, &[class::REPLICAS, data_type::BTREE, 1, 1, 0], &[sign(u64::from(node))]).await
    }

    /// Bucket `b` as `txn` would leave it: its alloc key, or a hole.
    pub(super) async fn bucket(&self, txn: &Txn, b: u64) -> Result<Bucket> {
        match self.get_in(txn, Btree::Alloc, Bpos::new(0, b, 0)).await? {
            Some((k, v)) if k.kind == kind::ALLOC_V4 => {
                let a = AllocV4::decode(&v)?;
                Ok(Bucket::of(&a, a.flags, a.io_time_write, a.journal_seq_nonempty))
            }
            Some(_) => Err(Error::Unsupported("alloc key before alloc_v4")),
            None => Ok(Bucket::HOLE),
        }
    }

    /// The keys a bucket's change from `before` to `after` writes: its alloc
    /// key, its freespace, LRU and bucket_gens entries, and its state's
    /// accounting (A6.12–A6.24, A7.12). An emptied bucket's alloc key and
    /// need_discard entry wait for the transaction's sequence.
    pub(super) async fn bucket_changed(&self, txn: &mut Txn, b: u64, before: Bucket, after: Bucket) -> Result<()> {
        txn.bucket_states.push((b, before, after));
        let bucket_sectors = self.sb.member.bucket_sectors as u32;
        let alloc_pos = Bpos::new(0, b, 0);
        if after.data_type == data_type::NEED_DISCARD && before.data_type != data_type::NEED_DISCARD {
            self.hold(txn, Btree::Alloc, 0, alloc_pos).await?;
            let next = {
                let st = self.st.borrow();
                (st.journal.open.as_ref().map(|o| o.seq), st.journal.next_seq)
            };
            for seq in [next.0, Some(next.1)].into_iter().flatten() {
                self.hold(txn, Btree::NeedDiscard, 0, Bpos::new(seq, b, 0)).await?;
            }
            txn.emptied.push((b, after));
        } else {
            self.put(txn, Btree::Alloc, 0, alloc_pos, Some((Key::new(kind::ALLOC_V4, alloc_pos), after.encode().to_vec()))).await?;
        }
        if before.data_type == data_type::FREE && after.data_type != data_type::FREE {
            self.take_free(txn, b).await?;
        }
        if before.data_type == data_type::NEED_DISCARD && after.data_type != data_type::NEED_DISCARD {
            let pos = Bpos::new(before.journal_seq_empty, b, 0);
            self.put(txn, Btree::NeedDiscard, 0, pos, None).await?;
        }
        let (old, new) = (before.lru(b, bucket_sectors), after.lru(b, bucket_sectors));
        if old != new {
            if let Some(pos) = old {
                self.put(txn, Btree::Lru, 0, pos, None).await?;
            }
            if let Some(pos) = new {
                self.put(txn, Btree::Lru, 0, pos, Some((Key::new(kind::SET, pos), Vec::new()))).await?;
            }
        }
        let (record, counters) = before.accounting(bucket_sectors);
        self.account(txn, &record, &counters.map(u64::wrapping_neg)).await?;
        let (record, counters) = after.accounting(bucket_sectors);
        self.account(txn, &record, &counters).await?;
        if before.gen != after.gen {
            let pos = Bpos::new(0, b >> 8, 0);
            let mut gens = match self.get_in(txn, Btree::BucketGens, pos).await? {
                Some((_, v)) if v.len() == value::BUCKET_GENS_BYTES => v,
                _ => alloc::vec![0u8; value::BUCKET_GENS_BYTES],
            };
            gens[(b & 255) as usize] = after.gen;
            self.put(txn, Btree::BucketGens, 0, pos, Some((Key::new(kind::BUCKET_GENS, pos), gens))).await?;
        }
        Ok(())
    }

    /// Takes free bucket `b` out of the freespace range that covers it.
    async fn take_free(&self, txn: &mut Txn, b: u64) -> Result<()> {
        const END: u64 = (1 << 56) - 1;
        let (k, _) = self.first_in(txn, Btree::Freespace, Bpos::new(0, b + 1, 0)).await?.ok_or(Error::Corrupt("a free bucket no freespace key covers"))?;
        let (end, bits) = (k.pos.offset & END, k.pos.offset & !END);
        let start = end.checked_sub(u64::from(k.size)).ok_or(Error::Corrupt("freespace key larger than its end"))?;
        if !(start..end).contains(&b) || k.pos.inode != 0 {
            return Err(Error::Corrupt("a free bucket no freespace key covers"));
        }
        let range = |from: u64, to: u64| {
            let pos = Bpos::new(0, to | bits, 0);
            Some((Key { size: (to - from) as u32, ..Key::new(kind::SET, pos) }, Vec::new()))
        };
        let below = (b > start).then(|| range(start, b));
        self.put(txn, Btree::Freespace, 0, k.pos, if end > b + 1 { range(b + 1, end) } else { None }).await?;
        if let Some(below) = below {
            self.put(txn, Btree::Freespace, 0, Bpos::new(0, b | bits, 0), below).await?;
        }
        Ok(())
    }

    /// A reclaimed node made reachable: an appended bset now counted in its
    /// node, or the node replaced by those written for it, each holding its
    /// share of the node's keys and of the changes since it was captured.
    pub(super) fn install(&self, st: &mut State, install: Install) {
        match install {
            Install::Append { id, sectors } => st.cache.change(id, |n| {
                n.written = sectors;
                n.on_disk = sectors;
                n.bsets += 1;
                n.captured = None;
            }),
            Install::Replace { ids, by, root } => {
                let mut keys = Vec::new();
                let mut unwritten = BTreeMap::new();
                for id in ids {
                    let btree = st.cache.change(id, |n| {
                        n.captured = None;
                        keys.extend(n.keys.iter().cloned());
                        unwritten.append(&mut n.unwritten);
                        n.btree
                    });
                    st.cache.remove(id);
                    if st.new_roots.get(&btree) == Some(&id) { st.new_roots.remove(&btree); }
                }
                for p in by {
                    let mine: Vec<(Key, Vec<u8>)> = keys.iter().filter(|(k, _)| (p.min..=p.max).contains(&k.pos)).cloned().collect();
                    let changes: BTreeMap<Bpos, u64> = unwritten.range(p.min..=p.max).map(|(k, v)| (*k, *v)).collect();
                    let id = st.cache.insert(fresh_cached(&p, mine));
                    let is_root = st.roots.get(&p.btree).is_some_and(|r| r.ptr.ptr.sector == p.sector);
                    let seq = st.journal.open.as_ref().map_or(st.journal.next_seq, |o| o.seq);
                    st.cache.change(id, |n| {
                        n.unwritten = changes;
                        if !is_root && n.bytes < self.minimum_fill() { n.compact = Some(seq); }
                    });
                }
                if let Some(p) = root {
                    let keys = p.keys.clone();
                    st.cache.insert(fresh_cached(&p, keys));
                }
            }
            Install::Collapse { id } => {
                st.cache.change(id, |n| { n.unwritten.clear(); n.compact = None; n.captured = None; });
                st.cache.remove(id);
            }
        }
    }

    /// Lays the replayed window over the nodes: each key installed in its
    /// node as a change of the entry it came from, from the top level down so
    /// each level's pointers lead to the next; each accounting delta newer
    /// than its stored key added (A7.8). Then reclaim writes until the dirty
    /// nodes are within the limit again.
    pub(super) async fn install_replay(&self, replayed: journal::Replay) -> Result<()> {
        self.st.borrow_mut().journal.replaying = true;
        let mut keys: Vec<_> = replayed.keys.into_iter().collect();
        keys.sort_by_key(|((btree, level, pos), _)| (core::cmp::Reverse(*level), *btree, *pos));
        for ((btree, level, pos), logged) in keys {
            if self.st.borrow().roots.get(&btree).is_some_and(|root| level > root.level) { continue; }
            let id = self.find(btree, level, pos).await?;
            self.st.borrow_mut().cache.change(id, |n| n.put(pos, logged.key, logged.seq));
        }
        for (pos, deltas) in replayed.accounting {
            let id = self.find(Btree::Accounting, 0, pos).await?;
            self.st.borrow_mut().cache.change(id, |n| {
                let i = n.at(pos);
                let (mut key, mut sums) = match n.keys.get(i) {
                    Some((k, v)) if k.pos == pos => (*k, journal::counters(v).expect("a cached accounting key decodes")),
                    _ => (Key::new(kind::ACCOUNTING, pos), Vec::new()),
                };
                let since = key.version;
                let mut first = None;
                for (version, delta) in deltas.iter().filter(|(v, _)| *v > since) {
                    sums.resize(sums.len().max(delta.len()), 0);
                    for (sum, d) in sums.iter_mut().zip(delta) {
                        *sum = sum.wrapping_add(*d);
                    }
                    key.version = *version;
                    let seq = u64::from(version.hi) << 32 | version.lo >> 32;
                    first = Some(first.map_or(seq, |f: u64| f.min(seq)));
                }
                if let Some(seq) = first {
                    n.put(pos, Some((key, value::counters(&sums))), seq);
                }
            });
        }
        {
            let st = self.st.borrow();
            let need = ((st.cache.dirty + st.cache.unpointed) as u64 * PER_NODE).div_ceil(st.journal.body() - st.journal.max_tx) + 1;
            if need > st.journal.ring_free() {
                return Err(Error::NoSpace("a journal too full to write out its replay"));
            }
        }
        while {
            let st = self.st.borrow();
            st.cache.dirty + st.cache.unpointed > st.limit
        } {
            self.reclaim_round(Order::OldestPin).await?;
        }
        self.flush_open().await?;
        self.st.borrow_mut().journal.replaying = false;
        Ok(())
    }
}

/// The bytes of the change at `pos` of `n` a bset carries: its key now, or
/// a `deleted` key hiding an older bset's.
fn changed(n: &Cached, pos: Bpos) -> Vec<u8> {
    match n.keys.get(n.at(pos)) {
        Some((k, v)) if k.pos == pos => k.encode(v),
        _ => Key::new(kind::DELETED, pos).encode(&[]),
    }
}

/// A node just written fresh, holding `keys`.
fn fresh_cached(p: &Planned, keys: Vec<(Key, Vec<u8>)>) -> Cached {
    Cached {
        btree: p.btree,
        level: p.level,
        min: p.min,
        max: p.max,
        seq: p.seq,
        sector: p.sector,
        gen: p.gen,
        written: p.sectors,
        on_disk: p.sectors,
        bsets: 1,
        bytes: keys.iter().map(|(_, v)| key_bytes(v)).sum(),
        keys: Rc::new(keys),
        unwritten: BTreeMap::new(),
        captured: None,
        compact: None,
        writing: false,
        fresh: false,
        held: 0,
        used: 0,
    }
}

/// Marks `[start, end)` in the member's btree bitmap, growing its shift
/// and folding its bits as far as `end` needs (A1.27). Whether it changed.
fn mark_bitmap(m: &mut Member, start: u64, end: u64) -> bool {
    if m.bitmap_covers(start, end) {
        return false;
    }
    let mut shift = u32::from(m.bitmap_shift);
    while end > 64 << shift {
        shift += 1;
    }
    let mut bitmap = 0u64;
    for i in 0..64 {
        if m.bitmap >> i & 1 != 0 {
            bitmap |= 1 << (i >> (shift - u32::from(m.bitmap_shift)));
        }
    }
    for i in start >> shift..end.div_ceil(1 << shift) {
        bitmap |= 1 << i;
    }
    m.bitmap = bitmap;
    m.bitmap_shift = shift as u8;
    true
}

#[cfg(test)]
impl Plan {
    pub(crate) fn merged_contains<D>(&self, m: &Mount<D>, btree: Btree, level: u8, pos: Bpos) -> bool {
        let st = m.st.borrow();
        self.ids.len() > 1 && self.btree == btree && self.level == level && self.ids.iter().any(|&id| {
            let n = st.cache.node(id);
            (n.min..=n.max).contains(&pos)
        })
    }

    /// Where an appended bset went: the node's sector, the bset's byte
    /// offset in it and its length.
    pub(crate) fn appended<D>(&self, m: &Mount<D>) -> Option<(u64, usize, usize)> {
        match &self.write {
            Write::Append { at, bytes } => Some((m.st.borrow().cache.node(self.ids[0]).sector, *at as usize * SECTOR, bytes.len())),
            Write::Fresh { .. } => None,
        }
    }

    /// The sectors of every node it wrote.
    pub(crate) fn sectors<D>(&self, m: &Mount<D>) -> Vec<u64> {
        match &self.write {
            Write::Append { .. } => alloc::vec![m.st.borrow().cache.node(self.ids[0]).sector],
            Write::Fresh { nodes, root } => nodes.iter().chain(root).map(|(p, _)| p.sector).collect(),
        }
    }
}
