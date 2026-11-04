//! Reclaim: dirty nodes written outside the slot, then made reachable by
//! a transaction on reclaim's own reserves.

use super::*;

pub(super) fn node_ptr(sector: u64, gen: u8) -> Ptr {
    Ptr { sector, dev: 0, gen, cached: false, unwritten: false }
}

/// The key of a pointer to `ptr`, a node whose max key is `max`.
pub(super) fn pointer(max: Bpos, ptr: &NodePtr) -> (Key, Vec<u8>) {
    (Key::new(kind::BTREE_PTR_V2, max), ptr.encode().to_vec())
}

/// Which dirty nodes reclaim writes first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    /// The oldest pin first, at any level: what frees journal buckets.
    OldestPin,
    /// The lowest level's first: how a clean unmount materializes what is
    /// left pending.
    LeavesFirst,
}

/// How a node's new writing is reached.
pub(super) enum Pointing<'a> {
    /// Its appended bsets, now `sectors` long in all.
    Append { sectors: u16 },
    /// New nodes replacing it, and a new root above them when a root split.
    Fresh { by: &'a [Planned], above: Option<&'a Planned> },
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
    Append { at: u64, bytes: Vec<u8>, journal_seq: u64 },
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
            k => fresh_cost(k) + (self.ids.len() - 1) as u64 * MERGE_EXTRA,
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
const MERGE_EXTRA: u64 = ALLOC + 4 * SET + BUCKET_GENS;

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
/// this many nodes of any size the writer accepts, each a key short of full.
/// Materialization takes at most one transaction of distinct key positions,
/// including during replay.
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

/// Every changed position may occupy a separate leaf. A common batch may
/// include two sibling rewrites; an oversized rewrite runs alone.
pub(super) const RECLAIM_ROOM: usize = {
    let pairs = BATCH * (fresh_nodes(2) + 7);
    let common = BATCH * fresh_nodes(COMMON_REWRITE);
    let oversized = fresh_nodes(MAX_REWRITE);
    let batch = if pairs > common { pairs } else { common };
    if batch > oversized { batch } else { oversized }
};

/// Nodes past the limit a batch's rewrites can leave dirty: their new
/// nodes, each taking the changes since its node was captured, beside the
/// node each replaces. A batch of common rewrites, or one alone.
pub(super) const REWRITTEN: usize = {
    let common = BATCH * (COMMON_REWRITE - 1);
    if common > MAX_REWRITE - 1 { common } else { MAX_REWRITE - 1 }
};

const _: () = assert!(fresh_cost(MAX_REWRITE) <= K_TX, "the largest rewrite fits one reclaim transaction");
const _: () = assert!(fresh_cost(2) + MERGE_EXTRA <= fresh_cost(MAX_REWRITE), "a sibling rewrite fits a pin's journal debt");

impl<D: BlockDev> Mount<D> {
    /// One batch of dirty nodes chosen by `order`, planned under the slot
    /// and written outside it; then their pointer updates journaled. The
    /// journal is written once that frees a bucket of its ring, so a sweep's
    /// updates share entries. Whether any work was retired or written.
    pub(super) async fn reclaim_round(&self, order: Order) -> Result<bool> {
        let had_work = self.busy();
        let plans = self.write_batch(order).await?;
        if plans.is_empty() {
            let retired = had_work && !self.busy();
            let published = if self.st.borrow().journal_frees() { self.flush_open().await? } else { false };
            return Ok(retired || published);
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
        let nodes = plans.iter().map(Plan::nodes).sum::<usize>();
        assert!(nodes <= RECLAIM_ROOM, "a reclaim batch exceeds its derived node bound");
        // Closed entries hold pipeline depth that only writing them returns:
        // reclaim never waits on what it would itself have to do.
        self.flush_closed().await?;
        // Reclaim's room for nodes is its own share; no draw limits it.
        self.admit(Class::Reclaim, Draw::Freeing, bound, nodes).await?;
        let _slot = self.slot(Class::Reclaim).await;
        self.prepare_pending(Class::Reclaim, bound, nodes)?;
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
        if only.is_none() {
            while self.materialize(order).await? && self.st.borrow().cache.dirty == 0 {}
        }
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
            let (btree, level, root, min, max, keys, changes, dependency, append, groups) = {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                let root = st.new_roots.get(&n.btree) == Some(&id) || st.roots.get(&n.btree).is_some_and(|r| r.ptr.ptr.sector == n.sector);
                let later = node::later_record_bytes(n.unwritten.keys().map(|pos| changed(n, *pos).len())).next_multiple_of(block);
                // An oversized node is split: an append would leave it so.
                let append = ids.len() == 1 && n.compact.is_none() && !n.fresh && n.written > 0 && !st.cache.oversized(n) && usize::from(n.on_disk) * SECTOR + later <= node_bytes;
                let keys: Vec<_> = ids.iter().flat_map(|&id| st.cache.node(id).keys.iter().cloned()).collect();
                let changes: BTreeMap<_, _> = ids.iter().flat_map(|&id| st.cache.node(id).unwritten.iter().map(|(&p, &s)| (p, s))).collect();
                let dependency = ids.iter().map(|&id| st.cache.node(id).journal_seq).max().unwrap();
                let sizes: Vec<usize> = keys.iter().map(|(_, v)| key_bytes(v)).collect();
                // At two thirds, as upstream splits; an oversized node, too
                // full for two at that, in as few as each a node holds.
                let mut groups = mkfs::spread(&sizes, mkfs::capacity(node_bytes) - FIRST_KEYS);
                if groups.len() > 2 {
                    groups = mkfs::spread(&sizes, node_bytes - FIRST_KEYS);
                }
                (n.btree, n.level, root, st.cache.node(ids[0]).min, st.cache.node(*ids.last().unwrap()).max, keys, changes, dependency, append, groups)
            };
            if groups.len() > MAX_GROUPS {
                return Err(Error::Unsupported("a btree node whose keys fill more than seven nodes"));
            }
            let written = groups.len() + usize::from(groups.len() > 1 && root);
            let cost = if append { APPEND_COST } else { fresh_cost(written) + (ids.len() - 1) as u64 * MERGE_EXTRA };
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
                let journal_seq = n.unwritten.values().map(|c| c.last).max().expect("a dirty node has changes").max(n.journal_seq);
                let at = u64::from(n.on_disk);
                let later = node::Later { btree, level, seq: n.seq, offset: at as usize * SECTOR, journal_seq, keys: &keys };
                let bytes = node::encode_later(block, self.key.as_ref(), &later)?;
                Write::Append { at, bytes, journal_seq }
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
                    let journal_seq = changes.range(low..=high).map(|(_, c)| c.last).max().unwrap_or(0).max(dependency);
                    nodes.push(self.fresh_node(btree, level, low, high, journal_seq, mine, sectors[j], self.nonzero()?, magic)?);
                }
                let above = (splits && root).then(|| {
                    let keys = nodes.iter().map(|(p, _): &(Planned, Vec<u8>)| pointer(p.max, &p.ptr())).collect();
                    let dependency = nodes.iter().map(|(p, _)| p.journal_seq).max().unwrap();
                    self.fresh_node(btree, level + 1, Bpos::MIN, Bpos::MAX, dependency, keys, sectors[groups.len()], self.nonzero()?, magic)
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

    /// Transfers a node's range with bounded positive growth. Deletions do
    /// not consume growth room; no allocation triggers repeat.
    pub(super) async fn materialize(&self, order: Order) -> Result<bool> {
        let next = {
            let st = self.st.borrow();
            match order {
                Order::OldestPin => st.pending.oldest().filter(|c| st.cache.oldest_pin().is_none_or(|p| c.first < p)),
                Order::LeavesFirst => (0..4).find_map(|level| Btree::ALL.iter().find_map(|&btree| {
                    st.pending.range(btree, level, Bpos::MIN, Bpos::MAX).next().map(|e| e.capture())
                })),
            }
        };
        let Some(next) = next else { return Ok(false) };
        let (btree, level, pos) = next.position;
        let id = self.find(btree, level, pos).await?;
        let mut st = self.st.borrow_mut();
        let n = st.cache.node(id);
        if n.writing || n.captured.is_some() || n.held != 0 { return Ok(false); }
        self.take_into(&mut st, id, pos)
    }

    /// Transfers pending keys of node `id`'s range into it, from `pos` on
    /// and round to it, with bounded positive growth. The caller has found
    /// the node free to change.
    pub(super) fn take_into(&self, st: &mut State, id: NodeId, pos: Bpos) -> Result<bool> {
        let n = st.cache.node(id);
        let (btree, level) = (n.btree, n.level);
        let mut captures = Vec::new();
        let mut growth = 0;
        let mut pending = st.pending.range(btree, level, pos, n.max)
            .chain(st.pending.range(btree, level, n.min, pos).take_while(|e| e.key().pos < pos)).peekable();
        while let Some(first) = pending.peek() {
            let at = first.key().pos;
            let start = captures.len();
            let before = n.keys.get(n.at(at)).filter(|(k, _)| k.pos == at).map_or(0, |(_, v)| key_bytes(v));
            let mut size = if btree == Btree::Accounting && level == 0 { before } else { 0 };
            while pending.peek().is_some_and(|e| e.key().pos == at) {
                let entry = pending.next().unwrap();
                if entry.key().kind != kind::DELETED { size = size.max(key_bytes(entry.value())); }
                captures.try_reserve(1).map_err(|_| Error::NoMemory)?;
                captures.push(entry.capture());
            }
            let added = size.saturating_sub(before);
            if growth + added > K_TX as usize * 8 && start != 0 { captures.truncate(start); continue; }
            growth += added;
        }
        let durable = st.journal.durable;
        let mut accounting = None;
        let transferred = !captures.is_empty();
        for capture in captures {
            let pos = capture.position.2;
            assert!(st.pending.transfer(capture, |entry| {
                st.cache.change(id, |n| {
                    if btree != Btree::Accounting || level != 0 {
                        let kv = (entry.key.kind != kind::DELETED).then_some((entry.key, entry.value));
                        let current = n.keys.get(n.at(pos)).filter(|(k, _)| k.pos == pos);
                        if entry.last <= durable && current == kv.as_ref() { return; }
                        n.put(pos, kv, entry.first, entry.last);
                        return;
                    }
                    let (mut key, mut sums) = match n.keys.get(n.at(pos)) {
                        Some((key, value)) if key.pos == pos => (*key, journal::counters(value).expect("cached accounting decodes")),
                        _ => (Key::new(kind::ACCOUNTING, pos), Vec::new()),
                    };
                    if accounting.is_none_or(|(previous, _)| previous != pos) { accounting = Some((pos, key.version)); }
                    if entry.key.version <= accounting.unwrap().1 { return; }
                    let delta = journal::counters(&entry.value).expect("pending accounting decodes");
                    sums.resize(sums.len().max(delta.len()), 0);
                    for (sum, d) in sums.iter_mut().zip(delta) { *sum = sum.wrapping_add(d); }
                    key.version = key.version.max(entry.key.version);
                    n.put(pos, Some((key, value::counters(&sums))), entry.first, entry.last);
                });
            }), "materialization cannot race a pending replacement under the slot");
        }
        Ok(transferred)
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
            // Root depth stays: older journal entries can still hold keys
            // at every former level. The sole-child spine is bounded by it.
            if n.min == Bpos::MIN && n.max == Bpos::MAX { return Ok(alloc::vec![id]); }
            if n.bytes >= self.minimum_fill() { return Ok(alloc::vec![id]); }
            (n.btree, n.level, n.max)
        };
        let parent = self.find(btree, level + 1, max).await?;
        let adjacent = {
            let mut st = self.st.borrow_mut();
            let keys = st.logical(parent, Bpos::MIN).collect::<Result<Vec<_>>>()?;
            let at = keys.partition_point(|(k, _)| k.pos < max);
            if keys.len() == 1 {
                let seq = st.journal.open.as_ref().map_or(st.journal.next_seq, |o| o.seq);
                st.cache.change(parent, |n| { n.compact.get_or_insert(seq); });
                return Ok(Vec::new());
            }
            let index = if at > 0 { at - 1 } else { at + 1 };
            let (k, v) = keys.get(index).ok_or(Error::Corrupt("underfull node absent from its parent"))?;
            (k.pos, NodePtr::decode(v)?)
        };
        let other = self.load(btree, level, &adjacent.1, adjacent.0).await?;
        let mut st = self.st.borrow_mut();
        let n = st.cache.node(other);
        if n.writing || n.captured.is_some() || n.held > 0 || st.cache.oversized(n) { return Ok(Vec::new()); }
        st.cache.change(other, |n| n.held += 1);
        Ok(if adjacent.0 < max { alloc::vec![other, id] } else { alloc::vec![id, other] })
    }

    /// A new node of `keys` and sequence `seq`, encoded as its one first
    /// record.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fresh_node(&self, btree: Btree, level: u8, min: Bpos, max: Bpos, journal_seq: u64, keys: Vec<(Key, Vec<u8>)>, at: (u64, u8), seq: u64, magic: u64) -> Result<(Planned, Vec<u8>)> {
        let encoded: Vec<Vec<u8>> = keys.iter().map(|(k, v)| k.encode(v)).collect();
        let build = Build { btree, level, seq, min, max, journal_seq, keys: &encoded };
        let bytes = node::encode(magic, self.sb.block_bytes(), self.key.as_ref(), &build)?;
        let sectors = (bytes.len() / SECTOR) as u16;
        Ok((Planned { btree, level, min, max, seq, sector: at.0, gen: at.1, sectors, journal_seq, keys }, bytes))
    }

    /// Room for a batch's new nodes now, outside the slot: buckets emptied
    /// made durable, and traversals that may still lead into them waited
    /// out. A volume short past that plans anyway; admission keeps room for
    /// reclaim, so a plan that finds no slot is a bug, and stops the mount.
    async fn make_room(&self) -> Result<()> {
        loop {
            let blocked = self.readers.borrow_mut().blocked();
            let (ready, returning, low) = {
                let st = self.st.borrow();
                (st.alloc.ready(blocked), st.alloc.returning(), Allocator::transient(st.cache.dirty + st.cache.unpointed + st.granted, st.reclaim_room, Draw::Freeing))
            };
            if !self.metadata_gc.get() && ready < low && Box::pin(self.compact_metadata()).await? {
                self.flush_open().await?;
                continue;
            }
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
    pub(super) async fn alloc_sector(&self) -> Result<(u64, u8)> {
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
                Write::Append { at, bytes, .. } => alloc::vec![(self.st.borrow().cache.node(p.ids[0]).sector + at, &bytes[..])],
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
            let (btree, level) = (plan.btree, plan.level);
            match plan.write {
                Write::Append { at, bytes, journal_seq } => {
                    let sectors = (at + (bytes.len() / SECTOR) as u64) as u16;
                    self.reach(txn, &plan.ids, btree, level, plan.root, Pointing::Append { sectors }).await?;
                    txn.installs.push(Install::Append { id: plan.ids[0], sectors, journal_seq });
                }
                Write::Fresh { nodes, root } => {
                    let by: Vec<Planned> = nodes.into_iter().map(|(p, _)| p).collect();
                    let root = root.map(|(p, _)| p);
                    self.reach(txn, &plan.ids, btree, level, plan.root, Pointing::Fresh { by: &by, above: root.as_ref() }).await?;
                    for p in by.iter().chain(&root) {
                        self.node_counted(txn, &mut buckets, btree, p.level, p.sector, p.gen, p.max, true).await?;
                    }
                    for &id in &plan.ids {
                        self.released(txn, &mut buckets, id).await?;
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

    /// The pointer updates that make `ids`' new writing reachable: their
    /// parent's keys, or their btree's root.
    pub(super) async fn reach(&self, txn: &mut Txn, ids: &[NodeId], btree: Btree, level: u8, root: bool, pointing: Pointing<'_>) -> Result<()> {
        let (min, max, seq, sector, gen) = {
            let st = self.st.borrow();
            let n = st.cache.node(ids[0]);
            (n.min, n.max, n.seq, n.sector, n.gen)
        };
        let root_key = Key::new(kind::BTREE_PTR_V2, Bpos::MAX);
        match pointing {
            Pointing::Append { sectors } => {
                let ptr = NodePtr { seq, written: sectors, min, ptr: node_ptr(sector, gen) };
                if root {
                    txn.roots.push(Root { btree, level, key: root_key, ptr });
                } else {
                    let (k, v) = pointer(max, &ptr);
                    self.put(txn, btree, level + 1, max, Some((k, v))).await?;
                }
            }
            Pointing::Fresh { by, above } => match (above, root) {
                (Some(r), _) => txn.roots.push(Root { btree, level: r.level, key: root_key, ptr: r.ptr() }),
                (None, true) => txn.roots.push(Root { btree, level, key: root_key, ptr: by[0].ptr() }),
                (None, false) => {
                    for &id in ids {
                        let old_max = self.st.borrow().cache.node(id).max;
                        self.put(txn, btree, level + 1, old_max, None).await?;
                    }
                    for p in by {
                        self.put(txn, btree, level + 1, p.max, Some(pointer(p.max, &p.ptr()))).await?;
                    }
                }
            },
        }
        Ok(())
    }

    /// Node `id`'s release from its bucket once a rewrite replaces it; none
    /// for a node never written.
    pub(super) async fn released(&self, txn: &mut Txn, buckets: &mut BTreeMap<u64, (Bucket, Bucket)>, id: NodeId) -> Result<()> {
        let (btree, level, sector, gen, max, written) = {
            let st = self.st.borrow();
            let n = st.cache.node(id);
            (n.btree, n.level, n.sector, n.gen, n.max, n.written)
        };
        if written > 0 {
            self.node_counted(txn, buckets, btree, level, sector, gen, max, false).await?;
        }
        Ok(())
    }

    /// A btree node at `sector` added to its bucket, or released from it:
    /// its bucket's change, its backpointer and its accounting.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn node_counted(&self, txn: &mut Txn, buckets: &mut BTreeMap<u64, (Bucket, Bucket)>, btree: Btree, level: u8, sector: u64, gen: u8, max: Bpos, added: bool) -> Result<()> {
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

    /// Published replacements take the sources' current keys, including
    /// every change since capture, partitioned by their new ranges.
    pub(super) fn install(&self, st: &mut State, install: Install) {
        match install {
            Install::Append { id, sectors, journal_seq } => st.cache.change(id, |n| {
                n.journal_seq = n.journal_seq.max(journal_seq);
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
                        unwritten.append(&mut n.unwritten.take());
                        n.btree
                    });
                    st.cache.remove(id);
                    if st.new_roots.get(&btree) == Some(&id) { st.new_roots.remove(&btree); }
                }
                for p in by {
                    let mine: Vec<(Key, Vec<u8>)> = keys.iter().filter(|(k, _)| (p.min..=p.max).contains(&k.pos)).cloned().collect();
                    let changes: Unwritten = unwritten.range(p.min..=p.max).map(|(k, v)| (*k, *v)).collect();
                    let id = st.cache.insert(fresh_cached(&p, mine));
                    let seq = st.journal.open.as_ref().map_or(st.journal.next_seq, |o| o.seq);
                    st.cache.change(id, |n| {
                        n.unwritten = changes;
                        if (n.min != Bpos::MIN || n.max != Bpos::MAX) && n.bytes < self.minimum_fill() { n.compact = Some(seq); }
                    });
                }
                if let Some(p) = root {
                    let keys = p.keys.clone();
                    st.cache.insert(fresh_cached(&p, keys));
                }
            }
        }
    }

    /// Replay has the same pending owner as runtime changes. Recover enough
    /// journal headroom for ordinary admission without loading every leaf.
    pub(super) async fn install_replay(&self, replayed: journal::Replay) -> Result<()> {
        {
            let mut st = self.st.borrow_mut();
            st.pending = replayed.pending;
            st.journal.replaying = true;
        }
        loop {
            let room = {
                let mut st = self.st.borrow_mut();
                if st.journal.reserve(K_TX, Class::Ordinary) {
                    st.journal.unreserve(K_TX);
                    true
                } else { false }
            };
            if room { break; }
            if !self.reclaim_round(Order::OldestPin).await? {
                return Err(Error::NoSpace("a journal too full to materialize its replay"));
            }
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
        journal_seq: p.journal_seq,
        bytes: keys.iter().map(|(_, v)| key_bytes(v)).sum(),
        keys: Rc::new(keys),
        unwritten: Unwritten::default(),
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
pub(super) fn mark_bitmap(m: &mut Member, start: u64, end: u64) -> bool {
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
            Write::Append { at, bytes, .. } => Some((m.st.borrow().cache.node(self.ids[0]).sector, *at as usize * SECTOR, bytes.len())),
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
