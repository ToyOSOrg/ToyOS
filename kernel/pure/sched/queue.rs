//! The per-CPU run queue: a real-time FIFO drained first, and the fair band.
//!
//! The fair band is EEVDF over the job groups this CPU holds, and EEVDF again
//! over each group's threads ([`Eevdf`] at both levels). A group's weight is
//! its job's weight times the group's share of the job's runnable threads
//! ([`Job::group_weight`]), refreshed at each decision. Lag goes with whatever
//! leaves the queue: a thread's to the next queue it enters, and a dissolving
//! group's with its last thread to the group that thread next enters. There
//! is no cross-CPU service accounting (owner decision J1).
//!
//! The queue owns its [`ReadyTask`] values. The running fair thread's entity
//! stays queued, charged as it runs, while its value is the CPU's `running`.

use alloc::collections::{BTreeMap, VecDeque};

use crate::sched::eevdf::{Eevdf, Lag};
use crate::sched::hw::Nanos;
use crate::sched::job::{Job, JobKey};
use crate::sched::sync::Arc;
use crate::sched::task::{ReadyTask, SchedPayload, Standing, TaskKey};

/// The request every fair thread and group issues: the longest a thread runs
/// before its CPU decides again, and what bounds the wait of one that queues
/// behind it.
pub const SLICE_NS: u64 = 4_000_000;

/// The least a fair thread runs before a woken thread or an earlier-due group
/// takes the CPU from it: what bounds the switching a stream of wakes costs.
pub const MIN_SLICE_NS: u64 = 250_000;

/// The most lag a thread or a group keeps while it is out of every queue, in
/// either direction: no sleeper credit beyond it, and no more debt.
pub const LAG_BOUND_NS: u64 = 4_000_000;

/// Every thread weighs the same inside its group.
const THREAD_WEIGHT: u64 = 1 << 16;

struct Group<X: SchedPayload> {
    job: Arc<Job>,
    threads: Eevdf<TaskKey>,
    /// Every queued thread's value but the running one's.
    ready: BTreeMap<TaskKey, ReadyTask<X>>,
}

pub struct RunQueue<X: SchedPayload> {
    rt: VecDeque<ReadyTask<X>>,
    groups: Eevdf<JobKey>,
    members: BTreeMap<JobKey, Group<X>>,
    /// The running thread, where it is fair: its entity is queued and its
    /// value is not.
    running: Option<(JobKey, TaskKey)>,
    ready_fair: usize,
    /// Negative-gate escape hatches only; see
    /// [`Self::set_lowest_key_within_group`] and [`Self::set_double_charge`].
    #[cfg(feature = "protocol-port")]
    lowest_key_within_group: bool,
    #[cfg(feature = "protocol-port")]
    double_charge: Option<JobKey>,
}

impl<X: SchedPayload> RunQueue<X> {
    pub fn new() -> Self {
        Self {
            rt: VecDeque::new(),
            groups: Eevdf::new(),
            members: BTreeMap::new(),
            running: None,
            ready_fair: 0,
            #[cfg(feature = "protocol-port")]
            lowest_key_within_group: false,
            #[cfg(feature = "protocol-port")]
            double_charge: None,
        }
    }

    /// Serve each group's lowest-keyed ready thread rather than its EEVDF
    /// pick, so one thread of a job takes all of the job's time on its CPU
    /// while its siblings wait. The simulator's progress invariant must catch
    /// it; `scenarios::fair_lowest_key_within_group` is the gate.
    #[cfg(feature = "protocol-port")]
    pub fn set_lowest_key_within_group(&mut self, on: bool) {
        self.lowest_key_within_group = on;
    }

    /// Charge `job`'s group twice for what it runs, so its job is throttled
    /// for work it never did. The simulator's I5 must catch it;
    /// `scenarios::fair_double_charge` is the gate.
    #[cfg(feature = "protocol-port")]
    pub fn set_double_charge(&mut self, job: Option<JobKey>) {
        self.double_charge = job;
    }

    /// Queue a task: the real-time band if it holds real-time treatment,
    /// otherwise its job's group, carrying the standing it left its last
    /// queue with.
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
        let standing = task.standing_mut();
        let carried = core::mem::take(&mut standing.group_lag);
        let left = standing.left.take();
        match self.members.get(&jk) {
            Some(_) if carried != Lag::ZERO => {
                let weight = self.groups.weight(jk);
                let mut held = self.groups.leave(jk);
                held.lag = held.lag.saturating_add(carried).clamp_ns(LAG_BOUND_NS);
                self.groups.rejoin(jk, weight, SLICE_NS, held);
            }
            Some(_) => {}
            None => {
                self.members.insert(jk, Group { job, threads: Eevdf::new(), ready: BTreeMap::new() });
                // Weighed below, once the thread is counted here.
                self.groups.join(jk, 1, SLICE_NS, carried);
            }
        }
        let group = self.members.get_mut(&jk).expect("just made");
        match left {
            Some(left) => group.threads.rejoin(key, THREAD_WEIGHT, SLICE_NS, left),
            None => group.threads.join(key, THREAD_WEIGHT, SLICE_NS, Lag::ZERO),
        };
        group.ready.insert(key, task);
        self.ready_fair += 1;
        self.reweigh(jk);
    }

    /// Take the entity of a fair thread out, writing what it takes with it
    /// into `standing`. The value is the caller's.
    fn leave(&mut self, jk: JobKey, key: TaskKey, standing: &mut Standing) {
        let group = self.members.get_mut(&jk).expect("a queued thread's group exists");
        let mut left = group.threads.leave(key);
        left.lag = left.lag.clamp_ns(LAG_BOUND_NS);
        standing.left = Some(left);
        group.job.leave();
        if group.threads.is_empty() {
            self.members.remove(&jk);
            standing.group_lag = self.groups.leave(jk).lag.clamp_ns(LAG_BOUND_NS);
        } else {
            self.reweigh(jk);
        }
    }

    /// Hold `jk`'s group at its current weight on this CPU.
    fn reweigh(&mut self, jk: JobKey) {
        let group = &self.members[&jk];
        let weight = group.job.group_weight(group.threads.len() as u32);
        self.groups.reweight(jk, weight);
    }

    /// Every group at the weight its job's runnable count now gives it: what
    /// the job's other CPUs changed since this one last decided.
    pub fn refresh(&mut self) {
        for (&jk, group) in &self.members {
            self.groups.reweight(jk, group.job.group_weight(group.threads.len() as u32));
        }
    }

    /// The fair band's pick: a group, then one of its threads.
    fn pick_fair(&mut self) -> Option<(JobKey, TaskKey)> {
        self.refresh();
        let jk = self.groups.pick()?;
        let group = &self.members[&jk];
        #[cfg(feature = "protocol-port")]
        if self.lowest_key_within_group {
            let key = group.threads.keys().min().expect("a queued group has a thread");
            return Some((jk, key));
        }
        Some((jk, group.threads.pick().expect("a queued group has a thread")))
    }

    /// The real-time band first, then the fair band's pick, which becomes the
    /// running thread. Asked only with nothing running.
    pub fn pop_next(&mut self) -> Option<ReadyTask<X>> {
        assert!(self.running.is_none(), "a pick with a fair thread still running");
        if let Some(task) = self.rt.pop_front() {
            return Some(task);
        }
        let (jk, key) = self.pick_fair()?;
        let task = self.members.get_mut(&jk).and_then(|g| g.ready.remove(&key)).expect("the pick has its value");
        self.ready_fair -= 1;
        self.running = Some((jk, key));
        Some(task)
    }

    /// Whether `key`, just queued, is what the fair band would run now in
    /// place of the running fair thread.
    pub fn would_preempt_for(&mut self, key: TaskKey) -> bool {
        self.running.is_some() && self.pick_fair().is_some_and(|(_, picked)| picked == key)
    }

    /// Whether the running thread's entity is the fair band's.
    pub fn running_is_fair(&self) -> bool {
        self.running.is_some()
    }

    /// The running fair thread received `ns`.
    pub fn charge_running(&mut self, ns: u64) {
        if let Some((jk, key)) = self.running {
            self.groups.charge(jk, ns);
            #[cfg(feature = "protocol-port")]
            if self.double_charge == Some(jk) {
                self.groups.charge(jk, ns);
            }
            self.members.get_mut(&jk).expect("the running thread's group").threads.charge(key, ns);
        }
    }

    /// The running task stops running and stays runnable: back to its band,
    /// which may have changed while it ran. A fair thread that `yielded` ends
    /// its own and its group's requests where they stand, so whatever was due
    /// after them runs first.
    pub fn put_back(&mut self, mut task: ReadyTask<X>, yielded: bool) {
        match self.running.take() {
            Some((jk, key)) => {
                assert_eq!(key, task.key(), "the task put back is not the running one");
                if task.rt().is_rt() {
                    self.leave(jk, key, task.standing_mut());
                    self.rt.push_back(task);
                } else {
                    if yielded {
                        self.groups.end_request(jk);
                        self.members.get_mut(&jk).expect("the running thread's group").threads.end_request(key);
                    }
                    self.members.get_mut(&jk).expect("the running thread's group").ready.insert(key, task);
                    self.ready_fair += 1;
                }
            }
            None => self.insert(task),
        }
    }

    /// The running task leaves the fair band without going back to it — it
    /// blocks, exits, stops or dies — taking its standing into `standing`.
    pub fn leave_running(&mut self, key: TaskKey, standing: &mut Standing) {
        if let Some((jk, running)) = self.running.take() {
            assert_eq!(running, key, "the task leaving is not the running one");
            self.leave(jk, key, standing);
        }
    }

    /// When the CPU must decide again for its running fair thread, charged up
    /// to `now` and running since `since`: the end of its own or its group's
    /// request, or a queued group whose request is due before its group's
    /// becoming eligible, once it has run [`MIN_SLICE_NS`].
    ///
    /// Within a group no such instant is needed: the group's request is as
    /// short as its threads', and deciding sooner only switches siblings.
    pub fn decision(&self, now: Nanos, since: Nanos) -> Option<Nanos> {
        let (jk, key) = self.running?;
        let threads = &self.members[&jk].threads;
        let left = self.groups.request_left_ns(jk).min(threads.request_left_ns(key));
        let mut at = now.after(left);
        if let Some(ns) = self.groups.overtaken_in(jk) {
            at = at.min(now.after(ns).max(since.after(MIN_SLICE_NS)));
        }
        Some(at)
    }

    /// The ready fair task a balance request is best answered with, out of
    /// the band: the one whose move from this CPU's summed group weight to the
    /// asker's `asking` brings the two closest, never `loaded` — the context
    /// this CPU still stands on (`SchedPass::answer_steal_requests`). An idle
    /// asker is answered with that task whatever the move does; a busy one only
    /// if the move narrows the difference by a quarter of the thread's weight,
    /// so that floor rounding cannot hand an odd split back and forth.
    pub fn pop_surplus(&mut self, loaded: Option<TaskKey>, asking: u64) -> Option<ReadyTask<X>> {
        self.refresh();
        let diff = self.weight() as i128 - asking as i128;
        let (jk, key) = self
            .members
            .iter()
            .filter_map(|(&jk, group)| {
                let key = group.ready.keys().rev().copied().find(|&key| Some(key) != loaded)?;
                let w = group.job.thread_weight() as i128;
                (asking == 0 || w + w / 4 <= diff).then_some(((diff - 2 * w).abs(), jk, key))
            })
            .min()
            .map(|(_, jk, key)| (jk, key))?;
        let mut task = self.members.get_mut(&jk).and_then(|g| g.ready.remove(&key)).expect("a ready task");
        self.ready_fair -= 1;
        self.leave(jk, key, task.standing_mut());
        Some(task)
    }

    /// Retire found the task queued rather than parked: out of whichever band
    /// it is in.
    pub fn remove(&mut self, key: TaskKey) -> Option<ReadyTask<X>> {
        if let Some(index) = self.rt.iter().position(|t| t.key() == key) {
            return self.rt.remove(index);
        }
        let jk = *self.members.iter().find(|(_, g)| g.ready.contains_key(&key))?.0;
        let mut task = self.members.get_mut(&jk).and_then(|g| g.ready.remove(&key)).expect("just found");
        self.ready_fair -= 1;
        self.leave(jk, key, task.standing_mut());
        Some(task)
    }

    /// Is an RT task waiting? The preemption decision in `finish()` and
    /// invariant I4's latency bound both hang off this.
    pub fn has_rt(&self) -> bool {
        !self.rt.is_empty()
    }

    /// Queued values, both bands.
    pub fn len(&self) -> usize {
        self.rt.len() + self.ready_fair
    }

    /// Queued fair values: the running fair thread is not one.
    pub fn fair_len(&self) -> usize {
        self.ready_fair
    }

    /// Is `key` queued in the fair band?
    pub fn holds_fair(&self, key: TaskKey) -> bool {
        self.members.values().any(|g| g.ready.contains_key(&key))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The summed group weights: what busy balancing compares.
    pub fn weight(&self) -> u64 {
        u64::try_from(self.groups.total_weight()).expect("a CPU's group weights sum past u64")
    }

    /// Each job's group here: its key, its weight and how many of its threads
    /// it holds, the running one included.
    pub fn groups(&self) -> impl Iterator<Item = (JobKey, u64, usize)> + '_ {
        self.members.iter().map(|(&jk, g)| (jk, self.groups.weight(jk), g.threads.len()))
    }

    /// Residents, for the invariant walks: the real-time band, then each
    /// group's ready threads.
    pub fn keys(&self) -> impl Iterator<Item = TaskKey> + '_ {
        self.tasks().map(|t| t.key())
    }

    pub fn tasks(&self) -> impl Iterator<Item = &ReadyTask<X>> + '_ {
        self.rt.iter().chain(self.members.values().flat_map(|g| g.ready.values()))
    }
}

impl<X: SchedPayload> Default for RunQueue<X> {
    fn default() -> Self {
        Self::new()
    }
}
