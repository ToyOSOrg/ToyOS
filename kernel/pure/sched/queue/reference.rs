//! Main's run queue and its EEVDF queue at 67713cbc1, less the protocol
//! port's escape hatches and what the test does not call: the oracle
//! [`super::differential`] holds every answer of the run queue to. A change
//! that means to decide differently deletes both.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use crate::sched::affinity::CpuMask;
use crate::sched::eevdf::{Lag, Left, MAX_WEIGHT, VSHIFT};
use crate::sched::hw::{CpuId, Nanos};
use crate::sched::job::{Job, JobKey, JOB_WEIGHT};
use crate::sched::queue::{LAG_BOUND_NS, MIN_SLICE_NS, SLICE_NS};
use crate::sched::sync::Arc;
use crate::sched::task::{ReadyTask, SchedPayload, Standing, TaskKey};

const THREAD_WEIGHT: u64 = 1 << 16;

fn group_weight(job: &Job, here: u32) -> u64 {
    let all = job.runnable();
    assert!(here <= all, "job {:?}: {here} threads queued here of {all} runnable", job.key());
    (JOB_WEIGHT * u64::from(here) / u64::from(all)).max(1)
}

const NIL: u32 = u32::MAX;

const MAX_HEIGHT: usize = 48;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot(u32);

struct Entity<K, P> {
    key: K,
    weight: u64,
    v: i128,
    rem: u64,
    vd: i128,
    slice: u64,
    seq: u64,
    left: u32,
    right: u32,
    height: u8,
    min_v: i128,
    payload: P,
}

pub struct Arena<K, P> {
    nodes: Vec<Option<Entity<K, P>>>,
    free: Vec<u32>,
}

pub struct Queue {
    root: u32,
    len: u32,
    total: i128,
    base: i128,
    sum: i128,
    seq: u64,
}

fn mul(a: i128, b: i128) -> i128 {
    a.checked_mul(b).expect("eevdf: arithmetic beyond i128")
}

fn add(a: i128, b: i128) -> i128 {
    a.checked_add(b).expect("eevdf: arithmetic beyond i128")
}

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

    pub fn payload(&self, s: Slot) -> &P {
        &self.node(s.0).payload
    }

    pub fn payload_mut(&mut self, s: Slot) -> &mut P {
        &mut self.node_mut(s.0).payload
    }

    pub fn request_left_ns(&self, s: Slot) -> u64 {
        let e = self.node(s.0);
        let left = mul(e.vd - e.v, e.weight as i128) - e.rem as i128;
        let ns = (left + (1 << VSHIFT) - 1) >> VSHIFT;
        ns.max(1) as u64
    }

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

    fn remove_first(&mut self, t: u32) -> (u32, u32) {
        let l = self.node(t).left;
        if l == NIL {
            return (t, self.node(t).right);
        }
        let (first, l) = self.remove_first(l);
        self.node_mut(t).left = l;
        (first, self.balance(t))
    }

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

    fn rebase(&mut self) {
        if self.total > 0 && !(0..self.total).contains(&self.sum) {
            let q = self.sum.div_euclid(self.total);
            self.base = add(self.base, q);
            self.sum -= mul(q, self.total);
        }
    }

    fn issue<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32, request: i128) {
        self.seq += 1;
        let seq = self.seq;
        let e = arena.node_mut(i);
        e.vd = add(e.v, request.div_euclid(e.weight as i128).max(1));
        e.seq = seq;
    }

    fn reissue<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32, request: i128) {
        let at = arena.order_of(i);
        self.root = arena.remove(self.root, at);
        self.issue(arena, i, request);
        self.root = arena.insert(self.root, i);
    }

    pub fn join<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, key: K, weight: u64, slice: u64, lag: Lag, payload: P) -> Slot {
        self.join_with_request(arena, key, weight, slice, lag, fine(slice), payload)
    }

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

    fn enter<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, i: u32, lag: Lag, request: i128) {
        let weight = arena.node(i).weight;
        assert!((1..=MAX_WEIGHT).contains(&weight), "eevdf: weight {weight} out of range");
        self.rebase();
        let w = weight as i128;
        let v_rel = if self.total == 0 {
            0
        } else {
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

    pub fn leave<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot) -> (Left, P) {
        let left = self.exit(arena, s.0);
        (left, arena.take(s.0).payload)
    }

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

    pub fn end_request<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot) {
        let slice = arena.node(s.0).slice;
        self.reissue(arena, s.0, fine(slice));
    }

    pub fn reweight<K: Copy, P>(&mut self, arena: &mut Arena<K, P>, s: Slot, weight: u64) {
        if arena.node(s.0).weight != weight {
            self.readmit(arena, s, weight, |left| left);
        }
    }

    pub fn slots<'a, K: Copy, P>(&self, arena: &'a Arena<K, P>) -> Slots<'a, K, P> {
        let mut stack = [NIL; MAX_HEIGHT];
        assert!(usize::from(arena.height(self.root)) < MAX_HEIGHT, "eevdf: a tree deeper than its index allows");
        let depth = usize::from(self.root != NIL);
        stack[0] = self.root;
        Slots { arena, stack, depth }
    }
}

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

struct Group {
    job: Arc<Job>,
    threads: Queue,
}

#[derive(Clone, Copy)]
struct Place {
    group: Slot,
    thread: Slot,
}

pub struct RunQueue<X: SchedPayload> {
    rt: VecDeque<ReadyTask<X>>,
    groups: Queue,
    group_slots: Arena<JobKey, Group>,
    threads: Arena<TaskKey, Option<ReadyTask<X>>>,
    members: BTreeMap<JobKey, Slot>,
    running: Option<Place>,
    ready_fair: usize,
    pinned_fair: usize,
    unrestricted_fair: usize,
}

impl<X: SchedPayload> RunQueue<X> {
    pub fn new() -> Self {
        Self {
            rt: VecDeque::new(),
            groups: Queue::new(),
            group_slots: Arena::new(),
            threads: Arena::new(),
            members: BTreeMap::new(),
            running: None,
            ready_fair: 0,
            pinned_fair: 0,
            unrestricted_fair: 0,
        }
    }

    pub fn insert(&mut self, task: ReadyTask<X>) {
        if task.rt().is_rt() {
            self.rt.push_back(task);
        } else {
            self.enter(task);
        }
    }

    fn enter(&mut self, mut task: ReadyTask<X>) {
        let key = task.key();
        let job = task.job().clone();
        let jk = job.key();
        job.enter();
        self.count_in(task.affinity());
        let standing = task.standing_mut();
        let carried = core::mem::take(&mut standing.group_lag);
        let left = standing.left.take();
        let group = match self.members.get(&jk) {
            Some(&group) => {
                if carried != Lag::ZERO {
                    let weight = self.group_slots.weight(group);
                    self.groups.readmit(&mut self.group_slots, group, weight, |mut held| {
                        held.lag = held.lag.saturating_add(carried).clamp_ns(LAG_BOUND_NS);
                        held
                    });
                }
                group
            }
            None => {
                let weight = group_weight(&job, 1);
                let group = self.groups.join(
                    &mut self.group_slots,
                    jk,
                    weight,
                    SLICE_NS,
                    carried,
                    Group { job, threads: Queue::new() },
                );
                self.members.insert(jk, group);
                group
            }
        };
        let threads = &mut self.group_slots.payload_mut(group).threads;
        match left {
            Some(left) => threads.rejoin(&mut self.threads, key, THREAD_WEIGHT, SLICE_NS, left, Some(task)),
            None => threads.join(&mut self.threads, key, THREAD_WEIGHT, SLICE_NS, Lag::ZERO, Some(task)),
        };
        self.ready_fair += 1;
        self.reweigh(group);
    }

    fn leave(&mut self, at: Place, standing: &mut Standing) {
        let group = self.group_slots.payload_mut(at.group);
        let (mut left, value) = group.threads.leave(&mut self.threads, at.thread);
        assert!(value.is_none(), "a thread left its queue holding its value");
        left.lag = left.lag.clamp_ns(LAG_BOUND_NS);
        standing.left = Some(left);
        group.job.leave();
        if group.threads.is_empty() {
            let (left, group) = self.groups.leave(&mut self.group_slots, at.group);
            self.members.remove(&group.job.key());
            standing.group_lag = left.lag.clamp_ns(LAG_BOUND_NS);
        } else {
            self.reweigh(at.group);
        }
    }

    fn reweigh(&mut self, group: Slot) {
        let g = self.group_slots.payload(group);
        let weight = group_weight(&g.job, g.threads.len() as u32);
        self.groups.reweight(&mut self.group_slots, group, weight);
    }

    pub fn refresh(&mut self) {
        for &group in self.members.values() {
            let g = self.group_slots.payload(group);
            let weight = group_weight(&g.job, g.threads.len() as u32);
            self.groups.reweight(&mut self.group_slots, group, weight);
        }
    }

    fn pick_fair(&mut self) -> Option<Place> {
        self.refresh();
        let group = self.groups.pick(&self.group_slots)?;
        let threads = &self.group_slots.payload(group).threads;
        let thread = threads.pick(&self.threads).expect("a queued group has a thread");
        Some(Place { group, thread })
    }

    pub fn pop_next(&mut self) -> Option<ReadyTask<X>> {
        assert!(self.running.is_none(), "a pick with a fair thread still running");
        if let Some(task) = self.rt.pop_front() {
            return Some(task);
        }
        let at = self.pick_fair()?;
        let task = self.threads.payload_mut(at.thread).take().expect("the pick has its value");
        self.ready_fair -= 1;
        self.count_out(task.affinity());
        self.running = Some(at);
        Some(task)
    }

    pub fn would_preempt_for(&mut self, key: TaskKey) -> bool {
        self.running.is_some() && self.pick_fair().is_some_and(|at| self.threads.key(at.thread) == key)
    }

    pub fn running_is_fair(&self) -> bool {
        self.running.is_some()
    }

    pub fn charge_running(&mut self, ns: u64) {
        if let Some(at) = self.running {
            self.groups.charge(&mut self.group_slots, at.group, ns);
            self.group_slots.payload_mut(at.group).threads.charge(&mut self.threads, at.thread, ns);
        }
    }

    pub fn put_back(&mut self, mut task: ReadyTask<X>, yielded: bool) {
        match self.running.take() {
            Some(at) => {
                assert_eq!(self.threads.key(at.thread), task.key(), "the task put back is not the running one");
                if task.rt().is_rt() {
                    self.leave(at, task.standing_mut());
                    self.rt.push_back(task);
                } else {
                    if yielded {
                        self.groups.end_request(&mut self.group_slots, at.group);
                        self.group_slots.payload_mut(at.group).threads.end_request(&mut self.threads, at.thread);
                    }
                    self.count_in(task.affinity());
                    *self.threads.payload_mut(at.thread) = Some(task);
                    self.ready_fair += 1;
                }
            }
            None => self.insert(task),
        }
    }

    pub fn leave_running(&mut self, key: TaskKey, standing: &mut Standing) {
        if let Some(at) = self.running.take() {
            assert_eq!(self.threads.key(at.thread), key, "the task leaving is not the running one");
            self.leave(at, standing);
        }
    }

    pub fn decision(&self, now: Nanos, since: Nanos) -> Option<Nanos> {
        let at = self.running?;
        let left = self.group_slots.request_left_ns(at.group).min(self.threads.request_left_ns(at.thread));
        let mut decide = now.after(left);
        if let Some(ns) = self.groups.overtaken_in(&self.group_slots, at.group) {
            decide = decide.min(now.after(ns).max(since.after(MIN_SLICE_NS)));
        }
        Some(decide)
    }

    fn ready(&self) -> impl Iterator<Item = (Place, &ReadyTask<X>)> + '_ {
        self.members.values().flat_map(move |&group| {
            self.group_slots.payload(group).threads.slots(&self.threads).filter_map(move |thread| {
                self.threads.payload(thread).as_ref().map(|task| (Place { group, thread }, task))
            })
        })
    }

    fn take(&mut self, at: Place) -> ReadyTask<X> {
        let mut task = self.threads.payload_mut(at.thread).take().expect("a ready task");
        self.ready_fair -= 1;
        self.count_out(task.affinity());
        self.leave(at, task.standing_mut());
        task
    }

    pub fn pop_surplus(&mut self, loaded: Option<TaskKey>, asking: u64, thief: CpuId) -> Option<ReadyTask<X>> {
        self.refresh();
        let diff = self.weight() as i128 - asking as i128;
        let (_, _, _, at) = self
            .members
            .iter()
            .filter_map(|(&jk, &group)| {
                let g = self.group_slots.payload(group);
                let (key, thread) = g
                    .threads
                    .slots(&self.threads)
                    .filter_map(|t| self.threads.payload(t).as_ref().map(|task| (task, t)))
                    .filter(|(task, _)| Some(task.key()) != loaded && task.affinity().contains(thief))
                    .map(|(task, t)| (task.key(), t))
                    .max_by_key(|&(key, _)| key)?;
                let w = g.job.thread_weight() as i128;
                (asking == 0 || w + w / 4 <= diff).then_some(((diff - 2 * w).abs(), jk, key, Place { group, thread }))
            })
            .min_by_key(|&(score, jk, key, _)| (score, jk, key))?;
        Some(self.take(at))
    }

    pub fn remove(&mut self, key: TaskKey) -> Option<ReadyTask<X>> {
        if let Some(index) = self.rt.iter().position(|t| t.key() == key) {
            return self.rt.remove(index);
        }
        let at = self.ready().find(|(_, task)| task.key() == key)?.0;
        Some(self.take(at))
    }

    pub fn has_rt(&self) -> bool {
        !self.rt.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rt.len() + self.ready_fair
    }

    pub fn fair_len(&self) -> usize {
        self.ready_fair
    }

    pub fn movable_fair(&self) -> usize {
        self.ready_fair - self.pinned_fair
    }

    pub fn takers(&self) -> CpuMask {
        if self.unrestricted_fair > 0 {
            return CpuMask::ANY;
        }
        self.ready()
            .map(|(_, task)| task.affinity())
            .filter(|mask| mask.single().is_none())
            .fold(CpuMask::NONE, CpuMask::union)
    }

    fn count_in(&mut self, mask: CpuMask) {
        self.pinned_fair += usize::from(mask.single().is_some());
        self.unrestricted_fair += usize::from(mask == CpuMask::ANY);
    }

    fn count_out(&mut self, mask: CpuMask) {
        self.pinned_fair -= usize::from(mask.single().is_some());
        self.unrestricted_fair -= usize::from(mask == CpuMask::ANY);
    }

    pub fn holds_movable(&self, key: TaskKey) -> bool {
        self.ready().any(|(_, task)| task.key() == key && task.affinity().single().is_none())
    }

    pub fn weight(&self) -> u64 {
        u64::try_from(self.groups.total_weight()).expect("a CPU's group weights sum past u64")
    }

    pub fn groups(&self) -> impl Iterator<Item = (JobKey, u64, usize)> + '_ {
        self.members
            .iter()
            .map(|(&jk, &group)| (jk, self.group_slots.weight(group), self.group_slots.payload(group).threads.len()))
    }

    pub fn keys(&self) -> impl Iterator<Item = TaskKey> + '_ {
        self.tasks().map(|t| t.key())
    }

    pub fn tasks(&self) -> impl Iterator<Item = &ReadyTask<X>> + '_ {
        self.rt.iter().chain(self.ready().map(|(_, task)| task))
    }
}
