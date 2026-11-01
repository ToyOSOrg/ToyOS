//! The per-CPU run queue: a real-time FIFO drained first, and the fair band.
//!
//! The fair band is EEVDF over the job groups this CPU holds, and EEVDF again
//! over each group's threads ([`Queue`] at both levels, their entities in this
//! queue's two arenas). A group's weight is its job's weight times the group's
//! share of the job's runnable threads
//! ([`Job::group_weight`]), refreshed at each decision. Lag goes with whatever
//! leaves the queue: a thread's to the next queue it enters, and a dissolving
//! group's with its last thread to the group that thread next enters. There
//! is no cross-CPU service accounting (owner decision J1).
//!
//! The queue owns its [`ReadyTask`] values. The running fair thread's entity
//! stays queued, charged as it runs, while its value is the CPU's `running`.

use alloc::collections::{BTreeMap, VecDeque};

use crate::sched::eevdf::{Arena, Lag, Queue, Slot};
use crate::sched::affinity::CpuMask;
use crate::sched::hw::{CpuId, Nanos};
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

/// A group entity's payload: its job, and the queue of its threads.
struct Group {
    job: Arc<Job>,
    threads: Queue,
}

/// The two entities a queued fair thread holds: its group's and its own.
#[derive(Clone, Copy)]
struct Place {
    group: Slot,
    thread: Slot,
}

pub struct RunQueue<X: SchedPayload> {
    rt: VecDeque<ReadyTask<X>>,
    groups: Queue,
    /// Every queued group's entity.
    group_slots: Arena<JobKey, Group>,
    /// Every queued thread's entity, whichever group's queue it is in, holding
    /// its value: `None` for the running thread's.
    threads: Arena<TaskKey, Option<ReadyTask<X>>>,
    members: BTreeMap<JobKey, Slot>,
    /// The running thread, where it is fair: its entity is queued and its
    /// value is not.
    running: Option<Place>,
    ready_fair: usize,
    /// Of `ready_fair`, those whose mask names one CPU: what no other CPU
    /// can be given. A queued task's mask does not change.
    pinned_fair: usize,
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
            groups: Queue::new(),
            group_slots: Arena::new(),
            threads: Arena::new(),
            members: BTreeMap::new(),
            running: None,
            ready_fair: 0,
            pinned_fair: 0,
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
        self.pinned_fair += usize::from(task.affinity().single().is_some());
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
                // At the weight it holds once this thread is counted here.
                let weight = job.group_weight(1);
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

    /// Take the entity of a fair thread out, writing what it takes with it
    /// into `standing`. Its value is the caller's, already taken.
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

    /// Hold a group at its current weight on this CPU.
    fn reweigh(&mut self, group: Slot) {
        let g = self.group_slots.payload(group);
        let weight = g.job.group_weight(g.threads.len() as u32);
        self.groups.reweight(&mut self.group_slots, group, weight);
    }

    /// Every group at the weight its job's runnable count now gives it: what
    /// the job's other CPUs changed since this one last decided.
    pub fn refresh(&mut self) {
        for &group in self.members.values() {
            let g = self.group_slots.payload(group);
            let weight = g.job.group_weight(g.threads.len() as u32);
            self.groups.reweight(&mut self.group_slots, group, weight);
        }
    }

    /// The fair band's pick: a group, then one of its threads.
    fn pick_fair(&mut self) -> Option<Place> {
        self.refresh();
        let group = self.groups.pick(&self.group_slots)?;
        let threads = &self.group_slots.payload(group).threads;
        #[cfg(feature = "protocol-port")]
        if self.lowest_key_within_group {
            let thread = threads
                .slots(&self.threads)
                .min_by_key(|&t| self.threads.key(t))
                .expect("a queued group has a thread");
            return Some(Place { group, thread });
        }
        let thread = threads.pick(&self.threads).expect("a queued group has a thread");
        Some(Place { group, thread })
    }

    /// The real-time band first, then the fair band's pick, which becomes the
    /// running thread. Asked only with nothing running.
    pub fn pop_next(&mut self) -> Option<ReadyTask<X>> {
        assert!(self.running.is_none(), "a pick with a fair thread still running");
        if let Some(task) = self.rt.pop_front() {
            return Some(task);
        }
        let at = self.pick_fair()?;
        let task = self.threads.payload_mut(at.thread).take().expect("the pick has its value");
        self.ready_fair -= 1;
        self.pinned_fair -= usize::from(task.affinity().single().is_some());
        self.running = Some(at);
        Some(task)
    }

    /// Whether `key`, just queued, is what the fair band would run now in
    /// place of the running fair thread.
    pub fn would_preempt_for(&mut self, key: TaskKey) -> bool {
        self.running.is_some() && self.pick_fair().is_some_and(|at| self.threads.key(at.thread) == key)
    }

    /// Whether the running thread's entity is the fair band's.
    pub fn running_is_fair(&self) -> bool {
        self.running.is_some()
    }

    /// The running fair thread received `ns`.
    pub fn charge_running(&mut self, ns: u64) {
        if let Some(at) = self.running {
            self.groups.charge(&mut self.group_slots, at.group, ns);
            #[cfg(feature = "protocol-port")]
            if self.double_charge == Some(self.group_slots.key(at.group)) {
                self.groups.charge(&mut self.group_slots, at.group, ns);
            }
            self.group_slots.payload_mut(at.group).threads.charge(&mut self.threads, at.thread, ns);
        }
    }

    /// The running task stops running and stays runnable: back to its band,
    /// which may have changed while it ran. A fair thread that `yielded` ends
    /// its own and its group's requests where they stand, so whatever was due
    /// after them runs first.
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
                    self.pinned_fair += usize::from(task.affinity().single().is_some());
                    *self.threads.payload_mut(at.thread) = Some(task);
                    self.ready_fair += 1;
                }
            }
            None => self.insert(task),
        }
    }

    /// The running task leaves the fair band without going back to it — it
    /// blocks, exits, stops or dies — taking its standing into `standing`.
    pub fn leave_running(&mut self, key: TaskKey, standing: &mut Standing) {
        if let Some(at) = self.running.take() {
            assert_eq!(self.threads.key(at.thread), key, "the task leaving is not the running one");
            self.leave(at, standing);
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
        let at = self.running?;
        let left = self.group_slots.request_left_ns(at.group).min(self.threads.request_left_ns(at.thread));
        let mut decide = now.after(left);
        if let Some(ns) = self.groups.overtaken_in(&self.group_slots, at.group) {
            decide = decide.min(now.after(ns).max(since.after(MIN_SLICE_NS)));
        }
        Some(decide)
    }

    /// Every queued fair thread whose value is here, with the group it is in.
    fn ready(&self) -> impl Iterator<Item = (Place, &ReadyTask<X>)> + '_ {
        self.members.values().flat_map(move |&group| {
            self.group_slots.payload(group).threads.slots(&self.threads).filter_map(move |thread| {
                self.threads.payload(thread).as_ref().map(|task| (Place { group, thread }, task))
            })
        })
    }

    /// Take a ready thread's value and its entity out of the band.
    fn take(&mut self, at: Place) -> ReadyTask<X> {
        let mut task = self.threads.payload_mut(at.thread).take().expect("a ready task");
        self.ready_fair -= 1;
        self.pinned_fair -= usize::from(task.affinity().single().is_some());
        self.leave(at, task.standing_mut());
        task
    }

    /// The ready fair task a balance request is best answered with, out of
    /// the band: the one whose move from this CPU's summed group weight to the
    /// asker's `asking` brings the two closest, never `loaded` — the context
    /// this CPU still stands on (`SchedPass::answer_steal_requests`), and
    /// never one whose mask leaves out `thief`. An idle
    /// asker is answered with that task whatever the move does; a busy one only
    /// if the move narrows the difference by a quarter of the thread's weight,
    /// so that floor rounding cannot hand an odd split back and forth. Each
    /// group offers its newest ready thread.
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

    /// Retire found the task queued rather than parked: out of whichever band
    /// it is in.
    pub fn remove(&mut self, key: TaskKey) -> Option<ReadyTask<X>> {
        if let Some(index) = self.rt.iter().position(|t| t.key() == key) {
            return self.rt.remove(index);
        }
        let at = self.ready().find(|(_, task)| task.key() == key)?.0;
        Some(self.take(at))
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

    /// Of [`Self::fair_len`], those whose mask names more than one CPU.
    pub fn movable_fair(&self) -> usize {
        self.ready_fair - self.pinned_fair
    }

    /// Every CPU a movable queued fair task may run on: the CPUs a push may
    /// ring for it.
    pub fn movable_mask(&self) -> CpuMask {
        self.ready()
            .map(|(_, task)| task.affinity())
            .filter(|mask| mask.single().is_none())
            .fold(CpuMask::NONE, CpuMask::union)
    }

    /// Is `key` queued in the fair band with a mask naming more than one CPU?
    pub fn holds_movable(&self, key: TaskKey) -> bool {
        self.ready().any(|(_, task)| task.key() == key && task.affinity().single().is_none())
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
        self.members
            .iter()
            .map(|(&jk, &group)| (jk, self.group_slots.weight(group), self.group_slots.payload(group).threads.len()))
    }

    /// Residents, for the invariant walks: the real-time band, then each
    /// group's ready threads.
    pub fn keys(&self) -> impl Iterator<Item = TaskKey> + '_ {
        self.tasks().map(|t| t.key())
    }

    pub fn tasks(&self) -> impl Iterator<Item = &ReadyTask<X>> + '_ {
        self.rt.iter().chain(self.ready().map(|(_, task)| task))
    }
}

impl<X: SchedPayload> Default for RunQueue<X> {
    fn default() -> Self {
        Self::new()
    }
}
