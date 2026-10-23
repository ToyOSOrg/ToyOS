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
    id: NodeId,
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
    /// u64s its pointer update journals at most: a parent key for an
    /// append; for new nodes their parent keys, and for each node allocated
    /// or released its alloc, freespace, LRU, need_discard, bucket_gens and
    /// backpointer keys, beside the accounting keys they share.
    fn cost(&self) -> u64 {
        match &self.write {
            Write::Append { .. } => APPEND_COST,
            Write::Fresh { nodes, root } => fresh_cost(nodes.len() + usize::from(root.is_some())),
        }
    }
}

const APPEND_COST: u64 = 16;

/// u64s the pointer update of a node rewritten to `nodes` new ones
/// journals at most. A measured two-way split journals 176.
pub(super) const fn fresh_cost(nodes: usize) -> u64 {
    112 + 64 * nodes as u64
}

impl<D: BlockDev> Mount<D> {
    /// One batch of dirty nodes chosen by `order`, planned
    /// under the slot and written outside it; then their pointer updates
    /// journaled. The journal is written once that frees a bucket of its
    /// ring, so a sweep's updates share entries. Whether there was any node
    /// to write.
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
        let plans = {
            let _slot = self.slot(Class::Reclaim).await;
            self.plan(order).await?
        };
        if plans.is_empty() {
            return Ok(plans);
        }
        self.write_plans(&plans).await?;
        {
            let mut st = self.st.borrow_mut();
            for p in &plans {
                st.cache.change(p.id, |n| n.writing = false);
            }
        }
        self.written.wake();
        Ok(plans)
    }

    /// Reclaim's second half: the written batch's pointer updates
    /// journaled, on reclaim's reserves and ahead in the slot.
    pub(crate) async fn point_batch(&self, plans: Vec<Plan>) -> Result<()> {
        let bound = plans.iter().map(Plan::cost).sum();
        let nodes = plans.len() + RECLAIM_LEAVES;
        // Closed entries hold pipeline depth that only writing them returns:
        // reclaim never waits on what it would itself have to do.
        self.flush_closed().await?;
        self.admit(Class::Reclaim, bound, nodes).await?;
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
    async fn plan(&self, order: Order) -> Result<Vec<Plan>> {
        let chosen: Vec<NodeId> = {
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
            let first = dirty.first().map(|d| (d.0, d.1));
            dirty.into_iter().filter(|d| Some((d.0, d.1)) == first).take(BATCH).map(|d| d.3).collect()
        };
        let node_bytes = self.sb.node_sectors() as usize * SECTOR;
        let block = self.sb.block_bytes();
        let magic = node::magic(&self.sb.header.internal_uuid);
        let mut plans = Vec::new();
        let mut spent = 0;
        for id in chosen {
            let (btree, level, root, append, groups) = {
                let st = self.st.borrow();
                let n = st.cache.node(id);
                let root = st.new_roots.get(&n.btree) == Some(&id) || st.roots.get(&n.btree).is_some_and(|r| r.ptr.ptr.sector == n.sector);
                let later = node::later_record_bytes(n.unwritten.keys().map(|pos| changed(n, *pos).len())).next_multiple_of(block);
                let append = !n.fresh && n.written > 0 && usize::from(n.on_disk) * SECTOR + later <= node_bytes;
                let sizes: Vec<usize> = n.keys.iter().map(|(_, v)| 8 * (KEY_U64S + v.len().div_ceil(8))).collect();
                let groups = mkfs::spread(&sizes, mkfs::capacity(node_bytes) - FIRST_KEYS);
                (n.btree, n.level, root, append, groups)
            };
            let cost = if append { APPEND_COST } else { fresh_cost(groups.len() + usize::from(groups.len() > 1 && root)) };
            if spent + cost > K_TX {
                assert!(!plans.is_empty(), "a node too large for one reclaim transaction to split");
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
                let st = self.st.borrow();
                let n = st.cache.node(id);
                let mut nodes = Vec::new();
                for (j, range) in groups.iter().enumerate() {
                    let min = if j == 0 { n.min } else { n.keys[range.start - 1].0.pos.successor().expect("a key below the last position") };
                    let max = if j + 1 == groups.len() { n.max } else { n.keys[range.end - 1].0.pos };
                    let keys = n.keys[range.clone()].to_vec();
                    let journal_seq = n.unwritten.range(min..=max).map(|(_, s)| *s).max().unwrap_or(0);
                    nodes.push(self.fresh_node(btree, level, min, max, journal_seq, keys, sectors[j], magic)?);
                }
                let above = (splits && root).then(|| {
                    let keys = nodes.iter().map(|(p, _): &(Planned, Vec<u8>)| pointer(p.max, &p.ptr())).collect();
                    self.fresh_node(btree, level + 1, Bpos::MIN, Bpos::MAX, 0, keys, sectors[groups.len()], magic)
                });
                Write::Fresh { nodes, root: above.transpose()? }
            };
            self.st.borrow_mut().cache.change(id, |n| {
                n.captured = n.unwritten.values().copied().min();
                n.unwritten.clear();
                n.writing = true;
            });
            plans.push(Plan { id, btree, level, root, write });
        }
        Ok(plans)
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

    /// The next free node slot, and its bucket's generation, from the open
    /// bucket or else the lowest free bucket the mount has not taken.
    async fn alloc_sector(&self) -> Result<(u64, u8)> {
        let bucket = u64::from(self.sb.member.bucket_sectors);
        let node = u64::from(self.sb.node_sectors());
        loop {
            {
                let mut st = self.st.borrow_mut();
                if let Some((b, slot, gen)) = st.alloc.open {
                    if slot < bucket / node {
                        st.alloc.open = Some((b, slot + 1, gen));
                        return Ok((b * bucket + slot * node, gen));
                    }
                }
            }
            let mut free = None;
            Source::walk(self, Btree::Freespace, Bpos::MIN, |k, _| {
                let end = k.pos.offset & ((1 << 56) - 1);
                let st = self.st.borrow();
                free = (end.saturating_sub(u64::from(k.size))..end).find(|b| !st.alloc.taken.contains(b));
                Ok(free.is_none())
            })
            .await?;
            let b = free.ok_or(Error::NoSpace("no free bucket for a btree node"))?;
            let gen = match Source::get(self, Btree::Alloc, Bpos::new(0, b, 0)).await? {
                Some((k, v)) if k.kind == kind::ALLOC_V4 => AllocV4::decode(&v)?.gen,
                _ => 0,
            };
            let mut st = self.st.borrow_mut();
            st.alloc.taken.insert(b);
            st.alloc.open = Some((b, 0, gen));
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
                Write::Append { at, bytes } => alloc::vec![(self.st.borrow().cache.node(p.id).sector + at, &bytes[..])],
                Write::Fresh { nodes, root } => nodes.iter().chain(root).map(|(planned, bytes)| (planned.sector, &bytes[..])).collect(),
            };
            for (sector, bytes) in writes {
                self.reissue("btree node write failed", async || mkfs::write(&self.dev, sector, bytes).await).await?;
                self.st.borrow_mut().clocks[1] += (bytes.len() / SECTOR) as u64;
            }
        }
        Ok(())
    }

    /// The batch's pointer updates, with the allocator and accounting keys
    /// of every node it added or released (A6, A7).
    async fn point(&self, txn: &mut Txn, plans: Vec<Plan>) -> Result<()> {
        let mut buckets: BTreeMap<u64, (Bucket, Bucket)> = BTreeMap::new();
        for plan in plans {
            let (min, max, seq, sector, gen, written) = {
                let st = self.st.borrow();
                let n = st.cache.node(plan.id);
                (n.min, n.max, n.seq, n.sector, n.gen, n.written)
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
                    txn.installs.push(Install::Append { id: plan.id, sectors });
                }
                Write::Fresh { nodes, root } => {
                    let by: Vec<Planned> = nodes.into_iter().map(|(p, _)| p).collect();
                    let root = root.map(|(p, _)| p);
                    match (&root, plan.root) {
                        (Some(r), _) => txn.roots.push(Root { btree, level: r.level, key: Key::new(kind::BTREE_PTR_V2, Bpos::MAX), ptr: r.ptr() }),
                        (None, true) => txn.roots.push(Root { btree, level, key: Key::new(kind::BTREE_PTR_V2, Bpos::MAX), ptr: by[0].ptr() }),
                        (None, false) => {
                            for p in &by {
                                self.put(txn, btree, level + 1, p.max, Some(pointer(p.max, &p.ptr()))).await?;
                            }
                        }
                    }
                    for p in by.iter().chain(&root) {
                        self.node_counted(txn, &mut buckets, btree, p.level, p.sector, p.gen, p.max, true).await?;
                    }
                    if written > 0 {
                        self.node_counted(txn, &mut buckets, btree, level, sector, gen, max, false).await?;
                    }
                    txn.installs.push(Install::Replace { id: plan.id, by, root });
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
            after.fill(node, clock);
        } else {
            after.drain(node);
        }
        let shift = match self.sb.flag(6, 40, 8) {
            0 => 10,
            s => s as u32,
        };
        let pos = Bpos::new(0, sector << shift, 0);
        let bp = added.then(|| (Key::new(kind::BACKPOINTER, pos), value::backpointer(btree as u8, level + 1, data_type::BTREE, gen, node, max).to_vec()));
        self.put(txn, Btree::Backpointers, 0, pos, bp).await?;
        let sign = |c: u64| if added { c } else { c.wrapping_neg() };
        let mut record = alloc::vec![class::BTREE];
        record.extend_from_slice(&(btree as u32).to_le_bytes());
        self.account(txn, &record, &[sign(u64::from(node)), sign(1), sign(u64::from(level > 0))]).await?;
        self.account(txn, &[class::REPLICAS, data_type::BTREE, 1, 1, 0], &[sign(u64::from(node))]).await
    }

    /// Bucket `b` as `txn` would leave it: its alloc key, or a hole.
    async fn bucket(&self, txn: &Txn, b: u64) -> Result<Bucket> {
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
    async fn bucket_changed(&self, txn: &mut Txn, b: u64, before: Bucket, after: Bucket) -> Result<()> {
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
                Some((_, v)) if v.len() == 256 => v,
                _ => alloc::vec![0u8; 256],
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
            Install::Replace { id, by, root } => {
                let (btree, keys, unwritten) = st.cache.change(id, |n| {
                    n.captured = None;
                    (n.btree, Rc::clone(&n.keys), core::mem::take(&mut n.unwritten))
                });
                st.cache.remove(id);
                if st.new_roots.get(&btree) == Some(&id) {
                    st.new_roots.remove(&btree);
                }
                for p in by {
                    let mine: Vec<(Key, Vec<u8>)> = keys.iter().filter(|(k, _)| (p.min..=p.max).contains(&k.pos)).cloned().collect();
                    let changes: BTreeMap<Bpos, u64> = unwritten.range(p.min..=p.max).map(|(k, v)| (*k, *v)).collect();
                    let id = st.cache.insert(fresh_cached(&p, mine));
                    st.cache.change(id, |n| n.unwritten = changes);
                }
                if let Some(p) = root {
                    let keys = p.keys.clone();
                    st.cache.insert(fresh_cached(&p, keys));
                }
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
    /// Where an appended bset went: the node's sector, the bset's byte
    /// offset in it and its length.
    pub(crate) fn appended<D>(&self, m: &Mount<D>) -> Option<(u64, usize, usize)> {
        match &self.write {
            Write::Append { at, bytes } => Some((m.st.borrow().cache.node(self.id).sector, *at as usize * SECTOR, bytes.len())),
            Write::Fresh { .. } => None,
        }
    }

    /// The sectors of every node it wrote.
    pub(crate) fn sectors<D>(&self, m: &Mount<D>) -> Vec<u64> {
        match &self.write {
            Write::Append { .. } => alloc::vec![m.st.borrow().cache.node(self.id).sector],
            Write::Fresh { nodes, root } => nodes.iter().chain(root).map(|(p, _)| p.sector).collect(),
        }
    }
}
