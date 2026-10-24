//! The btree nodes a mounted volume holds in memory, and what pins the
//! journal to them.
//!
//! **A node is dirty while it holds a change no written bset carries, and
//! unpointed while a written bset waits for its pointer update.** Neither
//! may be evicted: re-reading it would stop at the old written count and lose
//! the change. Each such node pins the journal at the oldest sequence of the
//! changes it holds, so the journal keeps them until they are durable in the
//! node and the node durable in its parent.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::bkey::{Bpos, Key};
use super::Btree;

/// A node's keys, in position order. Shared with readers, who hold them
/// across awaits; a change copies them when a reader still does.
pub(crate) type Keys = Rc<Vec<(Key, Vec<u8>)>>;

/// Where a node is in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NodeId(usize);

/// One node in memory.
pub(crate) struct Cached {
    pub btree: Btree,
    pub level: u8,
    pub min: Bpos,
    pub max: Bpos,
    /// Its random sequence, which every bset of it carries.
    pub seq: u64,
    pub sector: u64,
    /// The generation its pointer carries: its bucket's.
    pub gen: u8,
    /// Sectors the applied parent pointer names.
    pub written: u16,
    /// Sectors on the medium: where the next bset goes.
    pub on_disk: u16,
    /// Bsets on the medium.
    pub bsets: u16,
    pub keys: Keys,
    /// The bytes its keys take in a bset.
    pub bytes: usize,
    /// Positions changed since the last capture, each with the sequence of
    /// its first change since.
    pub unwritten: BTreeMap<Bpos, u64>,
    /// The oldest sequence a written bset holds whose pointer update is not
    /// applied.
    pub captured: Option<u64>,
    /// A write of it is in flight: it takes no change until it completes.
    pub writing: bool,
    /// Its next write goes to a fresh node, never appended: it was read
    /// from the medium after an unclean mount (§3.5).
    pub fresh: bool,
    /// Transactions between prepare and apply that will change it.
    pub held: u32,
    /// When it was last used, for eviction.
    pub used: u64,
}

impl Cached {
    pub(crate) fn pin(&self) -> Option<u64> {
        let oldest = self.unwritten.values().min().copied();
        match (oldest, self.captured) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub(crate) fn dirty(&self) -> bool {
        !self.unwritten.is_empty()
    }

    /// Whether dropping it loses nothing: a reread from the medium gives
    /// it back.
    fn evictable(&self) -> bool {
        !self.dirty() && self.captured.is_none() && !self.writing && self.held == 0 && self.written > 0
    }

    /// The index of the first key at or after `pos`.
    pub(crate) fn at(&self, pos: Bpos) -> usize {
        self.keys.partition_point(|(k, _)| k.pos < pos)
    }

    /// Sets the key at `pos`, or removes it; `seq` is the change's sequence.
    pub(crate) fn put(&mut self, pos: Bpos, kv: Option<(Key, Vec<u8>)>, seq: u64) {
        let keys = Rc::make_mut(&mut self.keys);
        let i = keys.partition_point(|(k, _)| k.pos < pos);
        let here = keys.get(i).is_some_and(|(k, _)| k.pos == pos);
        if here {
            self.bytes -= key_bytes(&keys[i].1);
        }
        if let Some((_, v)) = &kv {
            self.bytes += key_bytes(v);
        }
        match (kv, here) {
            (Some(kv), true) => keys[i] = kv,
            (Some(kv), false) => keys.insert(i, kv),
            (None, true) => {
                keys.remove(i);
            }
            (None, false) => {}
        }
        self.unwritten.entry(pos).or_insert(seq);
    }
}

/// The bytes a key with `value` takes in a bset.
pub(crate) fn key_bytes(value: &[u8]) -> usize {
    8 * (super::bkey::KEY_U64S + value.len().div_ceil(8))
}

/// Every cached node, by id and by sector, and the pins they hold.
pub(crate) struct Cache {
    nodes: Vec<Option<Cached>>,
    free: Vec<usize>,
    by_sector: BTreeMap<u64, NodeId>,
    /// Each pinning node's pin, counted.
    pins: BTreeMap<u64, usize>,
    /// Nodes past which clean ones are evicted.
    capacity: usize,
    pub dirty: usize,
    pub unpointed: usize,
    /// Changed nodes whose keys are past `full` bytes, which reclaim splits
    /// before any more is admitted. A clean node read past it, as a packed
    /// upstream leaf unpacks (A3.17), is not counted: no write is owed it.
    pub oversized: usize,
    full: usize,
    clock: u64,
}

impl Cache {
    /// A cache of `capacity` nodes, a node oversized past `full` bytes of
    /// keys.
    pub(crate) fn new(capacity: usize, full: usize) -> Self {
        Cache { nodes: Vec::new(), free: Vec::new(), by_sector: BTreeMap::new(), pins: BTreeMap::new(), capacity, dirty: 0, unpointed: 0, oversized: 0, full, clock: 0 }
    }

    pub(crate) fn oversized(&self, n: &Cached) -> bool {
        n.bytes > self.full
    }

    pub(crate) fn at_sector(&mut self, sector: u64) -> Option<NodeId> {
        let id = self.by_sector.get(&sector).copied()?;
        self.clock += 1;
        self.nodes[id.0].as_mut().unwrap().used = self.clock;
        Some(id)
    }

    /// Drops the nodes at sectors `[start, end)`, which no pointer reaches:
    /// a traversal read them through a pointer since replaced. `false`,
    /// dropping none, when one of them holds what the medium does not.
    pub(crate) fn drop_unreachable(&mut self, start: u64, end: u64) -> bool {
        let ids: Vec<NodeId> = self.by_sector.range(start..end).map(|(_, &id)| id).collect();
        if !ids.iter().all(|id| self.node(*id).evictable()) {
            return false;
        }
        for id in ids {
            let node = self.nodes[id.0].take().expect("a node of the cache");
            self.by_sector.remove(&node.sector);
            self.free.push(id.0);
        }
        true
    }

    /// The node at `sector`, without counting a use.
    pub(crate) fn id_at(&self, sector: u64) -> Option<NodeId> {
        self.by_sector.get(&sector).copied()
    }

    pub(crate) fn node(&self, id: NodeId) -> &Cached {
        self.nodes[id.0].as_ref().expect("a node of the cache")
    }

    /// Changes node `id` through `f`, keeping the pins and the dirty and
    /// unpointed counts.
    pub(crate) fn change<T>(&mut self, id: NodeId, f: impl FnOnce(&mut Cached) -> T) -> T {
        let node = self.nodes[id.0].as_mut().expect("a node of the cache");
        let full = self.full;
        let over = |n: &Cached| n.pin().is_some() && n.bytes > full;
        let (pin, dirty, unpointed, sector, over1) = (node.pin(), node.dirty(), node.captured.is_some(), node.sector, over(node));
        let out = f(node);
        let (pin2, dirty2, unpointed2, sector2, over2) = (node.pin(), node.dirty(), node.captured.is_some(), node.sector, over(node));
        self.oversized = self.oversized + usize::from(over2) - usize::from(over1);
        if pin != pin2 {
            if let Some(p) = pin {
                unpin(&mut self.pins, p);
            }
            if let Some(p) = pin2 {
                *self.pins.entry(p).or_default() += 1;
            }
        }
        self.dirty = self.dirty + usize::from(dirty2) - usize::from(dirty);
        self.unpointed = self.unpointed + usize::from(unpointed2) - usize::from(unpointed);
        if sector != sector2 {
            self.by_sector.remove(&sector);
            self.by_sector.insert(sector2, id);
        }
        out
    }

    /// The oldest sequence any node pins.
    pub(crate) fn oldest_pin(&self) -> Option<u64> {
        self.pins.keys().next().copied()
    }

    /// Adds `node`, first evicting the least recently used clean nodes to
    /// make room for it, so its id holds until the next insert.
    pub(crate) fn insert(&mut self, mut node: Cached) -> NodeId {
        assert!(node.pin().is_none(), "a node enters the cache with nothing pinned");
        self.evict();
        self.clock += 1;
        node.used = self.clock;
        let sector = node.sector;
        let id = match self.free.pop() {
            Some(i) => {
                self.nodes[i] = Some(node);
                NodeId(i)
            }
            None => {
                self.nodes.push(Some(node));
                NodeId(self.nodes.len() - 1)
            }
        };
        assert!(self.by_sector.insert(sector, id).is_none(), "two cached nodes at sector {sector}");
        id
    }

    /// Removes node `id`, which a rewrite has replaced: nothing pins it.
    pub(crate) fn remove(&mut self, id: NodeId) -> Cached {
        let node = self.nodes[id.0].take().expect("a node of the cache");
        assert!(node.pin().is_none(), "a replaced node still pins the journal");
        self.by_sector.remove(&node.sector);
        self.free.push(id.0);
        node
    }

    /// Evicts clean nodes until one more fits within the capacity, or none
    /// is left to evict.
    fn evict(&mut self) {
        let live = self.nodes.len() - self.free.len();
        if live < self.capacity {
            return;
        }
        let mut clean: Vec<(u64, usize)> = self.nodes.iter().enumerate().filter_map(|(i, n)| n.as_ref().filter(|n| n.evictable()).map(|n| (n.used, i))).collect();
        clean.sort_unstable();
        for (_, i) in clean.into_iter().take(live + 1 - self.capacity) {
            let node = self.nodes[i].take().unwrap();
            self.by_sector.remove(&node.sector);
            self.free.push(i);
        }
    }

    /// Every node's id.
    pub(crate) fn ids(&self) -> Vec<NodeId> {
        self.nodes.iter().enumerate().filter(|(_, n)| n.is_some()).map(|(i, _)| NodeId(i)).collect()
    }
}

fn unpin(pins: &mut BTreeMap<u64, usize>, p: u64) {
    let n = pins.get_mut(&p).expect("a pin counted");
    *n -= 1;
    if *n == 0 {
        pins.remove(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::kind;

    fn clean(sector: u64) -> Cached {
        Cached {
            btree: Btree::Extents,
            level: 0,
            min: Bpos::MIN,
            max: Bpos::MAX,
            seq: 1,
            sector,
            gen: 0,
            written: 1,
            on_disk: 1,
            bsets: 1,
            keys: Rc::new(Vec::new()),
            bytes: 0,
            unwritten: BTreeMap::new(),
            captured: None,
            writing: false,
            fresh: false,
            held: 0,
            used: 0,
        }
    }

    #[test]
    fn a_node_just_inserted_outlives_its_own_insert_when_only_it_is_clean() {
        let mut cache = Cache::new(1, usize::MAX);
        let dirty = cache.insert(clean(8));
        cache.change(dirty, |n| n.put(Bpos::MAX, Some((Key::new(kind::SET, Bpos::MAX), Vec::new())), 3));
        let new = cache.insert(clean(16));
        cache.change(new, |n| n.put(Bpos::MAX, None, 4));
        assert_eq!((cache.id_at(16), cache.dirty, cache.oldest_pin()), (Some(new), 2, Some(3)));
    }
}
