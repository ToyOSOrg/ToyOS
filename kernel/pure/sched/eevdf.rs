//! Earliest Eligible Virtual Deadline First over one queue of weighted
//! entities (Stoica and Abdel-Wahab, "Earliest Eligible Virtual Deadline
//! First", ODU TR-95-22, 1995). The same queue orders a CPU's job groups and
//! each group's threads.
//!
//! Each entity's virtual time `v` is the service it has received divided by its
//! weight. The queue's virtual time `V` is the weighted mean of its entities'
//! `v`, so lags `w·(V − v)` sum to zero by construction. An entity is eligible
//! when `v ≤ V`; the eligible entity with the earliest virtual deadline
//! `vd = v₀ + r/w` of its current request `r` runs. A join with lag `l` places
//! the entity so that its lag after the join is exactly `l` (the paper's
//! virtual-time adjustment); a leave reports the lag it takes away.
//!
//! Arithmetic is exact integers. Service is kept in units of `2^-VSHIFT` ns,
//! and each entity carries the remainder of its last division by its weight,
//! so charging never loses service. The only rounding is the queue's mean
//! (`V`), which is below one virtual unit: a lag read or a join is exact to
//! within `w · 2^-VSHIFT` ns, under `2^-8` ns at [`MAX_WEIGHT`].
//!
//! Entities sit in an AVL tree ordered by deadline, each node holding its
//! subtree's least `v`, so a pick, an eligibility query and every update are
//! O(log n) in the entities queued.
//!
//! A [`Queue`] is only the tree's root and its sums: its entities live in an
//! [`Arena`], which several queues may share, and a caller names one by the
//! [`Slot`] its join answered. So a queue costs nothing to make or drop, and
//! an entity joins and leaves without a lookup or an allocation once its
//! arena has held as many. [`Eevdf`] is one queue that names its entities by
//! key instead.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Fine service units per nanosecond, as a shift.
pub const VSHIFT: u32 = 48;

/// The largest weight an entity may carry. The products the queue forms are
/// about `|lag| · 2^VSHIFT · (total weight / smallest weight)`: they fit `i128`
/// while that ratio stays below `2^57` for lags of 4 ms, or `2^49` for a second.
/// They are checked, so a queue outside that fails loudly rather than wraps.
pub const MAX_WEIGHT: u64 = 1 << 40;

/// Service owed to (positive) or by (negative) an entity, in `2^-VSHIFT` ns.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Lag(pub i128);

impl Lag {
    pub const ZERO: Lag = Lag(0);

    pub fn from_ns(ns: i64) -> Lag {
        Lag((ns as i128) << VSHIFT)
    }

    /// Clamped to `±bound_ns`.
    pub fn clamp_ns(self, bound_ns: u64) -> Lag {
        let bound = (bound_ns as i128) << VSHIFT;
        Lag(self.0.clamp(-bound, bound))
    }

    pub fn saturating_add(self, other: Lag) -> Lag {
        Lag(self.0.saturating_add(other.0))
    }
}

const NIL: u32 = u32::MAX;

/// The deepest an AVL tree of `u32`-indexed nodes can be: `1.44·log2(2^32)`.
const MAX_HEIGHT: usize = 48;

/// One entity's place in its [`Arena`], valid from the join that answered it
/// to the leave that takes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot(u32);

struct Entity<K, P> {
    key: K,
    weight: u64,
    /// Absolute virtual time, in `2^VSHIFT`-scaled ns per unit weight.
    v: i128,
    /// Fine service charged but not yet a whole virtual unit: `< weight`.
    rem: u64,
    /// Virtual deadline of the current request.
    vd: i128,
    /// Request size in ns.
    slice: u64,
    /// Issue order of the current request: earlier wins a deadline tie.
    seq: u64,
    left: u32,
    right: u32,
    height: u8,
    /// The least `v` in this entity's subtree.
    min_v: i128,
    payload: P,
}

/// What [`Queue::leave`] hands back: everything needed to rejoin elsewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Left {
    pub lag: Lag,
    /// Fine service left in the current request.
    pub request_left: i128,
}

/// The entities of one or more queues, each carrying its caller's `P`.
pub struct Arena<K, P> {
    nodes: Vec<Option<Entity<K, P>>>,
    free: Vec<u32>,
}

/// One EEVDF queue over entities kept in an [`Arena`].
pub struct Queue {
    root: u32,
    len: u32,
    total: i128,
    /// Origin of `sum`, kept within one virtual unit of `V`.
    base: i128,
    /// `Σ w·(v − base)`; `V = base + sum / total`.
    sum: i128,
    seq: u64,
}

fn mul(a: i128, b: i128) -> i128 {
    a.checked_mul(b).expect("eevdf: arithmetic beyond i128")
}

fn add(a: i128, b: i128) -> i128 {
    a.checked_add(b).expect("eevdf: arithmetic beyond i128")
}

/// `ns` of service as fine units.
fn fine(ns: u64) -> i128 {
    (ns as i128) << VSHIFT
}

impl<K: Copy, P> Arena<K, P> {
    pub const fn new() -> Self {
        Self { nodes: Vec::new(), free: Vec::new() }
    }

    fn node(&self, i: u32) -> &Entity<K, P> {
        self.nodes[i as usize].as_ref().expect("eevdf: a slot no entity holds")
    }

    fn node_mut(&mut self, i: u32) -> &mut Entity<K, P> {
        self.nodes[i as usize].as_mut().expect("eevdf: a slot no entity holds")
    }

    fn take(&mut self, i: u32) -> Entity<K, P> {
        let e = self.nodes[i as usize].take().expect("eevdf: a slot no entity holds");
        self.free.push(i);
        e
    }

    fn put(&mut self, e: Entity<K, P>) -> u32 {
        match self.free.pop() {
            Some(i) => {
                self.nodes[i as usize] = Some(e);
                i
            }
            None => {
                self.nodes.push(Some(e));
                (self.nodes.len() - 1) as u32
            }
        }
    }

    pub fn key(&self, s: Slot) -> K {
        self.node(s.0).key
    }

    pub fn weight(&self, s: Slot) -> u64 {
        self.node(s.0).weight
    }

    pub fn slice(&self, s: Slot) -> u64 {
        self.node(s.0).slice
    }

    pub fn payload(&self, s: Slot) -> &P {
        &self.node(s.0).payload
    }

    pub fn payload_mut(&mut self, s: Slot) -> &mut P {
        &mut self.node_mut(s.0).payload
    }

    /// Service, in whole ns rounded up, until the entity's current request is done.
    pub fn request_left_ns(&self, s: Slot) -> u64 {
        let e = self.node(s.0);
        let left = mul(e.vd - e.v, e.weight as i128) - e.rem as i128;
        let ns = (left + (1 << VSHIFT) - 1) >> VSHIFT;
        ns.max(1) as u64
    }

    /// Tree order: deadline, then issue order.
    fn order_of(&self, i: u32) -> (i128, u64) {
        let e = self.node(i);
        (e.vd, e.seq)
    }

    fn height(&self, i: u32) -> u8 {
        if i == NIL { 0 } else { self.node(i).height }
    }

    fn update(&mut self, i: u32) {
        let (l, r) = (self.node(i).left, self.node(i).right);
        let mut min_v = self.node(i).v;
        if l != NIL {
            min_v = min_v.min(self.node(l).min_v);
        }
        if r != NIL {
            min_v = min_v.min(self.node(r).min_v);
        }
        let height = 1 + self.height(l).max(self.height(r));
        let e = self.node_mut(i);
        e.min_v = min_v;
        e.height = height;
    }

    fn rotate_right(&mut self, i: u32) -> u32 {
        let l = self.node(i).left;
        self.node_mut(i).left = self.node(l).right;
        self.node_mut(l).right = i;
        self.update(i);
        self.update(l);
        l
    }

    fn rotate_left(&mut self, i: u32) -> u32 {
        let r = self.node(i).right;
        self.node_mut(i).right = self.node(r).left;
        self.node_mut(r).left = i;
        self.update(i);
        self.update(r);
        r
    }

    fn balance(&mut self, i: u32) -> u32 {
        self.update(i);
        let (l, r) = (self.node(i).left, self.node(i).right);
        let skew = i16::from(self.height(l)) - i16::from(self.height(r));
        if skew > 1 {
            if self.height(self.node(l).left) < self.height(self.node(l).right) {
                let l = self.rotate_left(l);
                self.node_mut(i).left = l;
            }
            return self.rotate_right(i);
        }
        if skew < -1 {
            if self.height(self.node(r).right) < self.height(self.node(r).left) {
                let r = self.rotate_right(r);
                self.node_mut(i).right = r;
            }
            return self.rotate_left(i);
        }
        i
    }

    /// Insert the detached node `n` under `t`; answers the subtree's new root.
    fn insert(&mut self, t: u32, n: u32) -> u32 {
        if t == NIL {
            let e = self.node_mut(n);
            (e.left, e.right, e.height, e.min_v) = (NIL, NIL, 1, e.v);
            return n;
        }
        if self.order_of(n) < self.order_of(t) {
            let l = self.insert(self.node(t).left, n);
            self.node_mut(t).left = l;
        } else {
            let r = self.insert(self.node(t).right, n);
            self.node_mut(t).right = r;
        }
        self.balance(t)
    }

    /// Detach the node at tree order `at` from under `t`.
    fn remove(&mut self, t: u32, at: (i128, u64)) -> u32 {
        assert!(t != NIL, "eevdf: a queued entity is missing from the tree");
        let here = self.order_of(t);
        if at < here {
            let l = self.remove(self.node(t).left, at);
            self.node_mut(t).left = l;
        } else if at > here {
            let r = self.remove(self.node(t).right, at);
            self.node_mut(t).right = r;
        } else {
            let (l, r) = (self.node(t).left, self.node(t).right);
            if l == NIL {
                return r;
            }
            if r == NIL {
                return l;
            }
            let (next, rest) = self.remove_first(r);
            let e = self.node_mut(next);
            (e.left, e.right) = (l, rest);
            return self.balance(next);
        }
        self.balance(t)
    }

    /// Detach the first node under `t`: answers it and the subtree's new root.
    fn remove_first(&mut self, t: u32) -> (u32, u32) {
        let l = self.node(t).left;
        if l == NIL {
            return (t, self.node(t).right);
        }
        let (first, l) = self.remove_first(l);
        self.node_mut(t).left = l;
        (first, self.balance(t))
    }

    /// Recompute the least `v` on the path to the node at `at`, whose `v`
    /// changed and whose place did not.
    fn refresh(&mut self, t: u32, at: (i128, u64)) {
        let here = self.order_of(t);
        if at < here {
            self.refresh(self.node(t).left, at);
        } else if at > here {
            self.refresh(self.node(t).right, at);
        }
        self.update(t);
    }
}

impl<K: Copy, P> Default for Arena<K, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub const fn new() -> Self {
        Self { root: NIL, len: 0, total: 0, base: 0, sum: 0, seq: 0 }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn total_weight(&self) -> u128 {
        self.total as u128
    }

    fn eligible(&self, v: i128) -> bool {
        mul(self.total, v - self.base) <= self.sum
    }

    /// Move `base` to `⌊V⌋` so the sums stay small.
    fn rebase(&mut self) {
        // `0 ≤ sum < total` is `⌊V⌋ = base` already, and the division is spared.
        if self.total > 0 && !(0..self.total).contains(&self.sum) {
            let q = self.sum.div_euclid(self.total);
            self.base = add(self.base, q);
            self.sum -= mul(q, self.total);
        }
    }

    /// Set a detached node's request.
    fn issue<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32, request: i128) {
        self.seq += 1;
        let seq = self.seq;
        let e = arena.node_mut(i);
        e.vd = add(e.v, request.div_euclid(e.weight as i128).max(1));
        e.seq = seq;
    }

    /// Give node `i` a fresh request, moving it to its new place in the tree.
    fn reissue<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32, request: i128) {
        let at = arena.order_of(i);
        self.root = arena.remove(self.root, at);
        self.issue(arena, i, request);
        self.root = arena.insert(self.root, i);
    }

    /// Enter with `lag` and a fresh request of `slice` ns: the entity holds
    /// `lag` itself afterwards up to rounding, or zero in an empty queue, where
    /// there is no one to owe or be owed.
    pub fn join<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, key: K, weight: u64, slice: u64, lag: Lag, payload: P) -> Slot {
        self.join_with_request(arena, key, weight, slice, lag, fine(slice), payload)
    }

    /// Enter carrying what [`Self::leave`] returned elsewhere: the lag and the
    /// unfinished part of the request.
    pub fn rejoin<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, key: K, weight: u64, slice: u64, left: Left, payload: P) -> Slot {
        let request = left.request_left.clamp(1, fine(slice));
        self.join_with_request(arena, key, weight, slice, left.lag, request, payload)
    }

    #[allow(clippy::too_many_arguments)]
    fn join_with_request<K: Copy, P>(
        &mut self,
        arena: &mut Arena<K, P>,
        key: K,
        weight: u64,
        slice: u64,
        lag: Lag,
        request: i128,
        payload: P,
    ) -> Slot {
        let i = arena.put(Entity {
            key,
            weight,
            v: 0,
            rem: 0,
            vd: 0,
            slice,
            seq: 0,
            left: NIL,
            right: NIL,
            height: 1,
            min_v: 0,
            payload,
        });
        self.enter(arena, i, lag, request);
        Slot(i)
    }

    /// Place the detached node `i`, whose weight and slice are set, with
    /// `lag` and a fresh request of `request` fine units.
    fn enter<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32, lag: Lag, request: i128) {
        let weight = arena.node(i).weight;
        assert!((1..=MAX_WEIGHT).contains(&weight), "eevdf: weight {weight} out of range");
        self.rebase();
        let w = weight as i128;
        let v_rel = if self.total == 0 {
            0
        } else {
            // v = V − l/W − l/w makes w·(V' − v) = l once V' includes v.
            let l = lag.0;
            let num = mul(w, self.sum - l) - mul(l, self.total);
            num.div_euclid(mul(self.total, w))
        };
        let e = arena.node_mut(i);
        e.v = add(self.base, v_rel);
        e.rem = 0;
        self.issue(arena, i, request);
        self.root = arena.insert(self.root, i);
        self.sum = add(self.sum, mul(w, v_rel));
        self.total += w;
        self.len += 1;
    }

    /// Detach node `i` from the tree and the sums, answering what it takes
    /// away; the node stays in its arena.
    fn exit<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32) -> Left {
        self.rebase();
        let lag = self.lag_of(arena, i);
        let at = arena.order_of(i);
        self.root = arena.remove(self.root, at);
        let e = arena.node(i);
        let w = e.weight as i128;
        let (v, vd) = (e.v, e.vd);
        self.sum -= mul(w, v - self.base);
        self.total -= w;
        self.len -= 1;
        if self.total == 0 {
            self.sum = 0;
        }
        Left { lag, request_left: mul(vd - v, w) }
    }

    /// Leave, taking the entity's lag and unfinished request with it, and its
    /// payload out of the arena.
    pub fn leave<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot) -> (Left, P) {
        let left = self.exit(arena, s.0);
        (left, arena.take(s.0).payload)
    }

    /// Leave and enter again in place at `weight`, carrying the lag and the
    /// unfinished request as `adjust` answers them: a reweight, or a lag
    /// brought from elsewhere. The slot is the same.
    pub fn readmit<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot, weight: u64, adjust: impl FnOnce(Left) -> Left) {
        let left = adjust(self.exit(arena, s.0));
        let e = arena.node_mut(s.0);
        e.weight = weight;
        let request = left.request_left.clamp(1, fine(e.slice));
        self.enter(arena, s.0, left.lag, request);
    }

    fn lag_of<K: Copy, P>(&self, arena: &Arena<K, P>, i: u32) -> Lag {
        let e = arena.node(i);
        if self.total == 0 {
            return Lag::ZERO;
        }
        let w = e.weight as i128;
        let lag = mul(w, self.sum - mul(self.total, e.v - self.base)).div_euclid(self.total);
        Lag(lag)
    }

    /// `w·(V − v)`.
    pub fn lag<K: Copy, P>(&self, arena: &Arena<K, P>, s: Slot) -> Lag {
        self.lag_of(arena, s.0)
    }

    /// The running entity received `ns`. A finished request is followed at
    /// once by the next, whose deadline counts from where service now stands.
    pub fn charge<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot, ns: u64) {
        let i = s.0;
        let e = arena.node_mut(i);
        let total = fine(ns) + e.rem as i128;
        let w = e.weight as i128;
        let dv = total / w;
        e.rem = (total % w) as u64;
        e.v = add(e.v, dv);
        let (v, vd, slice) = (e.v, e.vd, e.slice);
        self.sum = add(self.sum, mul(dv, w));
        if v >= vd {
            self.reissue(arena, i, fine(slice));
        } else {
            let at = arena.order_of(i);
            arena.refresh(self.root, at);
        }
    }

    /// The eligible entity with the earliest deadline.
    pub fn pick<K: Copy, P>(&self, arena: &Arena<K, P>) -> Option<Slot> {
        let mut t = self.root;
        while t != NIL {
            let e = arena.node(t);
            if e.left != NIL && self.eligible(arena.node(e.left).min_v) {
                t = e.left;
            } else if self.eligible(e.v) {
                return Some(Slot(t));
            } else if e.right != NIL && self.eligible(arena.node(e.right).min_v) {
                t = e.right;
            } else {
                break;
            }
        }
        assert!(self.is_empty(), "eevdf: a non-empty queue with nothing eligible");
        None
    }

    /// Service, in whole ns of `running` being served, until a queued entity
    /// whose request is due before `running`'s becomes eligible and so the
    /// pick: `V` advances by `1 / total` per unit of service, and an entity
    /// with lag `l < 0` becomes eligible once it has advanced `−l / w`.
    pub fn overtaken_in<K: Copy, P>(&self, arena: &Arena<K, P>, running: Slot) -> Option<u64> {
        let before = arena.order_of(running.0);
        let mut least: Option<i128> = None;
        let mut t = self.root;
        while t != NIL {
            let e = arena.node(t);
            if arena.order_of(t) < before {
                let here = match e.left {
                    NIL => e.v,
                    l => e.v.min(arena.node(l).min_v),
                };
                least = Some(least.map_or(here, |m| m.min(here)));
                t = e.right;
            } else {
                t = e.left;
            }
        }
        least.map(|v| {
            let behind = mul(self.total, v - self.base) - self.sum;
            ((behind.max(0) + (1 << VSHIFT) - 1) >> VSHIFT) as u64
        })
    }

    /// End the entity's current request where its service stands, and issue
    /// the next: what a voluntary yield gives up.
    pub fn end_request<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot) {
        let slice = arena.node(s.0).slice;
        self.reissue(arena, s.0, fine(slice));
    }

    /// Change weight, keeping lag and the service left in the current request.
    pub fn reweight<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot, weight: u64) {
        if arena.node(s.0).weight != weight {
            self.readmit(arena, s, weight, |left| left);
        }
    }

    /// Change the request size; the current request is replaced by one of the
    /// new size if that ends sooner.
    pub fn set_slice<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot, slice: u64) {
        let i = s.0;
        let e = arena.node_mut(i);
        e.slice = slice;
        let request = mul(e.vd - e.v, e.weight as i128);
        if fine(slice) < request {
            self.reissue(arena, i, fine(slice));
        }
    }

    /// Every entity queued, in no particular order.
    pub fn slots<'a, K: Copy, P>(&self, arena: &'a Arena<K, P>) -> Slots<'a, K, P> {
        let mut stack = [NIL; MAX_HEIGHT];
        assert!(usize::from(arena.height(self.root)) < MAX_HEIGHT, "eevdf: a tree deeper than its index allows");
        let depth = usize::from(self.root != NIL);
        stack[0] = self.root;
        Slots { arena, stack, depth }
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

/// [`Queue::slots`]: a walk holding at most the right children along one path.
pub struct Slots<'a, K, P> {
    arena: &'a Arena<K, P>,
    stack: [u32; MAX_HEIGHT],
    depth: usize,
}

impl<K: Copy, P> Iterator for Slots<'_, K, P> {
    type Item = Slot;

    fn next(&mut self) -> Option<Slot> {
        let t = *self.stack.get(self.depth.checked_sub(1)?)?;
        self.depth -= 1;
        let e = self.arena.node(t);
        for child in [e.right, e.left] {
            if child != NIL {
                self.stack[self.depth] = child;
                self.depth += 1;
            }
        }
        Some(Slot(t))
    }
}

/// One queue whose entities are named by key, with an arena of its own.
pub struct Eevdf<K> {
    arena: Arena<K, ()>,
    queue: Queue,
    slots: BTreeMap<K, Slot>,
}

impl<K: Copy + Ord> Eevdf<K> {
    pub fn new() -> Self {
        Self { arena: Arena::new(), queue: Queue::new(), slots: BTreeMap::new() }
    }

    fn slot(&self, key: K) -> Slot {
        *self.slots.get(&key).expect("eevdf: entity is not queued")
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn contains(&self, key: K) -> bool {
        self.slots.contains_key(&key)
    }

    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.slots.keys().copied()
    }

    pub fn total_weight(&self) -> u128 {
        self.queue.total_weight()
    }

    pub fn weight(&self, key: K) -> u64 {
        self.arena.weight(self.slot(key))
    }

    /// [`Queue::join`], answering the lag the entity really holds afterwards.
    pub fn join(&mut self, key: K, weight: u64, slice: u64, lag: Lag) -> Lag {
        assert!(!self.contains(key), "eevdf: entity joined twice");
        let s = self.queue.join(&mut self.arena, key, weight, slice, lag, ());
        self.slots.insert(key, s);
        self.queue.lag(&self.arena, s)
    }

    pub fn leave(&mut self, key: K) -> Left {
        let s = self.slots.remove(&key).expect("eevdf: entity is not queued");
        self.queue.leave(&mut self.arena, s).0
    }

    /// [`Queue::rejoin`], answering the lag the entity really holds afterwards.
    pub fn rejoin(&mut self, key: K, weight: u64, slice: u64, left: Left) -> Lag {
        assert!(!self.contains(key), "eevdf: entity joined twice");
        let s = self.queue.rejoin(&mut self.arena, key, weight, slice, left, ());
        self.slots.insert(key, s);
        self.queue.lag(&self.arena, s)
    }

    pub fn lag(&self, key: K) -> Lag {
        self.queue.lag(&self.arena, self.slot(key))
    }

    pub fn charge(&mut self, key: K, ns: u64) {
        let s = self.slot(key);
        self.queue.charge(&mut self.arena, s, ns);
    }

    pub fn pick(&self) -> Option<K> {
        self.queue.pick(&self.arena).map(|s| self.arena.key(s))
    }

    pub fn overtaken_in(&self, running: K) -> Option<u64> {
        self.queue.overtaken_in(&self.arena, self.slot(running))
    }

    pub fn request_left_ns(&self, key: K) -> u64 {
        self.arena.request_left_ns(self.slot(key))
    }

    pub fn end_request(&mut self, key: K) {
        let s = self.slot(key);
        self.queue.end_request(&mut self.arena, s);
    }

    pub fn reweight(&mut self, key: K, weight: u64) {
        let s = self.slot(key);
        self.queue.reweight(&mut self.arena, s, weight);
    }

    pub fn set_slice(&mut self, key: K, slice: u64) {
        let s = self.slot(key);
        self.queue.set_slice(&mut self.arena, s, slice);
    }

    pub fn slice(&self, key: K) -> u64 {
        self.arena.slice(self.slot(key))
    }

    /// `Σ lag`, which is zero up to the mean's rounding: the conservation the
    /// whole construction rests on, for tests.
    pub fn lag_sum(&self) -> Lag {
        Lag(self.keys().map(|k| self.lag(k).0).sum())
    }
}

impl<K: Copy + Ord> Default for Eevdf<K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const MS: u64 = 1_000_000;
    const UNIT: u64 = 1 << 16;

    /// A deterministic generator; the core has no dependency to borrow one from.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Run the queue's choice for `ns`, to the end of its request or `step`,
    /// whichever comes first. Returns the entity and the service it got.
    fn run(q: &mut Eevdf<u32>, step: u64) -> (u32, u64) {
        let k = q.pick().unwrap();
        let ns = q.request_left_ns(k).min(step);
        q.charge(k, ns);
        (k, ns)
    }

    #[test]
    fn equal_weights_share_equally_and_lags_sum_to_zero() {
        let mut q = Eevdf::new();
        for k in 0..4 {
            q.join(k, UNIT, 3 * MS, Lag::ZERO);
        }
        let mut got = [0u64; 4];
        for _ in 0..4000 {
            let (k, ns) = run(&mut q, u64::MAX);
            got[k as usize] += ns;
        }
        for g in got {
            assert_eq!(g, 3000 * MS);
        }
        assert!(q.lag_sum().0.abs() < 4 * (UNIT as i128));
    }

    /// A short request that ran ahead of its share is due before the long one
    /// now running; `overtaken_in` is exactly the service after which it is
    /// eligible and so picked.
    #[test]
    fn overtaken_in_is_the_service_until_an_earlier_request_becomes_the_pick() {
        let mut rng = Lcg(11);
        for _trial in 0..200 {
            let mut q = Eevdf::new();
            // Within a factor of 4, so the short request's deadline is first.
            q.join(0u32, UNIT / 2 + rng.below(UNIT * 3 / 2), MS / 4, Lag::ZERO);
            q.join(1, UNIT / 2 + rng.below(UNIT * 3 / 2), 4 * MS, Lag::ZERO);
            q.charge(0, MS / 4);
            let long = 1;
            assert_eq!(q.pick(), Some(long));
            let ns = q.overtaken_in(long).unwrap();
            assert!(ns > 0 && ns < q.request_left_ns(long));
            q.charge(long, ns - 1);
            assert_eq!(q.pick(), Some(long));
            q.charge(long, 1);
            assert_eq!(q.pick(), Some(0));
            assert_eq!(q.overtaken_in(0), None);
        }
    }

    /// The paper's Theorem 3 bounds for a steady set joining at zero lag:
    /// `−r_i < lag_i < max(r_max, q)`, with `q` the charge granularity — here
    /// the scheduler stops each run at its request's end, so `q` is the step.
    #[test]
    fn lags_stay_inside_the_papers_bounds_for_random_weights_and_requests() {
        let mut rng = Lcg(7);
        for _trial in 0..50 {
            let n = 2 + rng.below(10) as u32;
            let mut q = Eevdf::new();
            let mut slices = Vec::new();
            for k in 0..n {
                let w = 1 + rng.below(1 << 20);
                let r = MS / 4 + rng.below(4 * MS);
                slices.push(r);
                q.join(k, w, r, Lag::ZERO);
            }
            let r_max = *slices.iter().max().unwrap();
            let step = MS / 2 + rng.below(MS);
            let rounding = 1 << 30; // far above the 2^-8 ns error, far below 1 ns
            for _ in 0..2000 {
                run(&mut q, step);
                for k in 0..n {
                    let lag = q.lag(k).0;
                    assert!(lag > -fine(slices[k as usize]) - rounding, "lag {lag} below −r");
                    assert!(lag < fine(r_max.max(step)) + rounding, "lag {lag} above max(r, q)");
                }
            }
        }
    }

    #[test]
    fn weights_set_the_split() {
        let mut q = Eevdf::new();
        q.join(0, UNIT, 2 * MS, Lag::ZERO);
        q.join(1, 3 * UNIT, 2 * MS, Lag::ZERO);
        let mut got = [0u64; 2];
        for _ in 0..10_000 {
            let (k, ns) = run(&mut q, u64::MAX);
            got[k as usize] += ns;
        }
        let total = got[0] + got[1];
        // Within one request of the exact 1:3 split.
        assert!((got[0] as i128 - total as i128 / 4).abs() <= 2 * MS as i128);
    }

    #[test]
    fn a_shorter_request_changes_order_not_share() {
        let mut q = Eevdf::new();
        q.join(0, UNIT, MS / 4, Lag::ZERO);
        q.join(1, UNIT, 4 * MS, Lag::ZERO);
        let mut got = [0u64; 2];
        let mut longest_wait_for_0 = 0;
        let mut since_0 = 0;
        for _ in 0..20_000 {
            let (k, ns) = run(&mut q, u64::MAX);
            got[k as usize] += ns;
            if k == 0 {
                longest_wait_for_0 = longest_wait_for_0.max(since_0);
                since_0 = 0;
            } else {
                since_0 += ns;
            }
        }
        let total = got[0] + got[1];
        assert!((got[0] as i128 - total as i128 / 2).abs() <= 4 * MS as i128);
        // The long request is served in pieces no longer than its own size,
        // and the short one never waits a whole long request twice over.
        assert!(longest_wait_for_0 <= 4 * MS);
    }

    #[test]
    fn a_join_holds_exactly_the_lag_it_brought_and_a_leave_takes_it() {
        let mut q = Eevdf::new();
        q.join(0, UNIT, 3 * MS, Lag::ZERO);
        q.join(1, 2 * UNIT, 3 * MS, Lag::ZERO);
        for _ in 0..7 {
            run(&mut q, MS);
        }
        for lag_ns in [-3 * MS as i64, -1, 0, 1, 2 * MS as i64] {
            let held = q.join(9, 5 * UNIT, 3 * MS, Lag::from_ns(lag_ns));
            assert!((held.0 - Lag::from_ns(lag_ns).0).abs() <= 5 * UNIT as i128 + 1);
            assert!(q.lag_sum().0.abs() <= 8 * UNIT as i128);
            let left = q.leave(9);
            assert_eq!(left.lag, held);
            assert!(q.lag_sum().0.abs() <= 8 * UNIT as i128);
        }
    }

    #[test]
    fn an_empty_queue_has_no_one_to_owe() {
        let mut q: Eevdf<u32> = Eevdf::new();
        assert_eq!(q.join(0, UNIT, MS, Lag::from_ns(-(MS as i64))), Lag::ZERO);
    }

    #[test]
    fn reweight_keeps_lag_and_the_request_left() {
        let mut q = Eevdf::new();
        q.join(0, UNIT, 3 * MS, Lag::ZERO);
        q.join(1, UNIT, 3 * MS, Lag::ZERO);
        q.charge(0, MS);
        let (lag, left) = (q.lag(0), q.request_left_ns(0));
        q.reweight(0, 7 * UNIT);
        assert!((q.lag(0).0 - lag.0).abs() <= 8 * UNIT as i128);
        assert!(q.request_left_ns(0).abs_diff(left) <= 1);
        assert_eq!(q.weight(0), 7 * UNIT);
    }

    /// The tree's shape and augmentation, and its answers against a scan of
    /// every entity, after each of a long random run of joins, leaves,
    /// charges, reweights and slice changes.
    #[test]
    fn the_tree_answers_as_a_scan_of_every_entity_does() {
        fn scan_pick(q: &Eevdf<u32>) -> Option<u32> {
            q.slots
                .values()
                .map(|s| q.arena.node(s.0))
                .filter(|e| q.queue.eligible(e.v))
                .min_by_key(|e| (e.vd, e.seq))
                .map(|e| e.key)
        }
        fn scan_overtaken(q: &Eevdf<u32>, running: u32) -> Option<u64> {
            let r = q.arena.node(q.slot(running).0);
            q.slots
                .values()
                .map(|s| q.arena.node(s.0))
                .filter(|e| (e.vd, e.seq) < (r.vd, r.seq))
                .map(|e| {
                    let behind = mul(q.queue.total, e.v - q.queue.base) - q.queue.sum;
                    ((behind.max(0) + (1 << VSHIFT) - 1) >> VSHIFT) as u64
                })
                .min()
        }
        /// Height, least `v` and the ordered keys of the subtree at `t`.
        fn walk(q: &Eevdf<u32>, t: u32, out: &mut Vec<(i128, u64)>) -> (u8, i128) {
            if t == NIL {
                return (0, i128::MAX);
            }
            let e = q.arena.node(t);
            let (hl, ml) = walk(q, e.left, out);
            out.push((e.vd, e.seq));
            let (hr, mr) = walk(q, e.right, out);
            assert!(hl.abs_diff(hr) <= 1, "an unbalanced node");
            assert_eq!(e.height, 1 + hl.max(hr));
            assert_eq!(e.min_v, e.v.min(ml).min(mr));
            (e.height, e.min_v)
        }
        let mut rng = Lcg(23);
        let mut q: Eevdf<u32> = Eevdf::new();
        let mut running: Option<u32> = None;
        for step in 0..20_000 {
            match rng.below(10) {
                0 | 1 if q.len() < 64 => {
                    let k = (0..).find(|k| !q.contains(*k)).unwrap();
                    let lag = rng.below(8 * MS) as i64 - 4 * MS as i64;
                    q.join(k, 1 + rng.below(1 << 24), MS / 4 + rng.below(4 * MS), Lag::from_ns(lag));
                }
                2 if !q.is_empty() => {
                    let k = q.keys().nth(rng.below(q.len() as u64) as usize).unwrap();
                    let left = q.leave(k);
                    if running == Some(k) {
                        running = None;
                    }
                    if rng.below(2) == 0 {
                        q.rejoin(k, 1 + rng.below(1 << 24), MS / 4 + rng.below(4 * MS), left);
                    }
                }
                3 if !q.is_empty() => {
                    let k = q.keys().nth(rng.below(q.len() as u64) as usize).unwrap();
                    q.reweight(k, 1 + rng.below(1 << 24));
                }
                4 if !q.is_empty() => {
                    let k = q.keys().nth(rng.below(q.len() as u64) as usize).unwrap();
                    q.set_slice(k, MS / 4 + rng.below(4 * MS));
                }
                _ => {
                    if running.is_none() {
                        running = q.pick();
                    }
                    if let Some(k) = running {
                        q.charge(k, 1 + rng.below(q.request_left_ns(k)));
                        if rng.below(3) == 0 {
                            running = None;
                        }
                    }
                }
            }
            let mut ordered = Vec::new();
            walk(&q, q.queue.root, &mut ordered);
            let mut walked: Vec<u32> = q.queue.slots(&q.arena).map(|s| s.0).collect();
            let mut held: Vec<u32> = q.slots.values().map(|s| s.0).collect();
            walked.sort_unstable();
            held.sort_unstable();
            assert_eq!(walked, held, "step {step}: the walk is not every queued slot once");
            assert!(ordered.windows(2).all(|w| w[0] < w[1]), "step {step}: out of deadline order");
            assert_eq!(ordered.len(), q.len());
            assert_eq!(q.pick(), scan_pick(&q), "step {step}: pick");
            if let Some(k) = running {
                assert_eq!(q.overtaken_in(k), scan_overtaken(&q, k), "step {step}: overtaken_in");
            }
        }
    }

    #[test]
    fn charging_loses_no_service_to_rounding() {
        // A weight that divides nothing: 10^6 one-nanosecond charges must add
        // up to exactly 10^6 ns of virtual progress times the weight.
        let mut q = Eevdf::new();
        let w = (1 << 40) - 3;
        q.join(0, w, 3 * MS, Lag::ZERO);
        q.join(1, w, 3 * MS, Lag::ZERO);
        let lag_before = q.lag(0);
        for _ in 0..1_000_000 {
            q.charge(0, 1);
        }
        q.charge(1, 1_000_000);
        // Both received 1 ms; their lags are back where they started up to
        // the mean's rounding, one weight's worth of fine units.
        assert!((q.lag(0).0 - lag_before.0).abs() <= 2 * w as i128);
    }
}
