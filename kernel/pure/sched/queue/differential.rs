//! The run queue against [`super::reference`], the one it replaced, over
//! generated schedules: each operation answers the same in both, and every
//! query of every queue and every standing a task leaves with is the same
//! after it. The schedules hold several jobs whose threads spread over up to
//! three queues, so group weights are not all powers of two, beside real-time
//! threads and one-CPU, two-CPU and unrestricted masks.

use alloc::vec::Vec;

use super::{reference, RunQueue};
use crate::sched::affinity::CpuMask;
use crate::sched::eevdf::{Lag, Left};
use crate::sched::hw::{CpuId, Nanos};
use crate::sched::job::{Job, JobKey, JOB_WEIGHT};
use crate::sched::mailbox::NoPreempt;
use crate::sched::sync::Arc;
use crate::sched::task::{ReadyTask, RtState, SchedPayload, TaskBuilder, TaskKey};

struct Payload;

impl SchedPayload for Payload {
    type Ctx = ();

    fn name(&self) -> u64 {
        0
    }
}

type Task = ReadyTask<Payload>;

/// What a test can ask a queue without changing it.
#[derive(PartialEq, Debug)]
struct View {
    len: usize,
    fair_len: usize,
    movable_fair: usize,
    takers: CpuMask,
    weight: u64,
    groups: Vec<(JobKey, u64, usize)>,
    keys: Vec<TaskKey>,
    movable: Vec<TaskKey>,
    has_rt: bool,
    running_is_fair: bool,
    decision: Option<Nanos>,
}

/// Both run queues, operation for operation.
trait Band {
    fn insert(&mut self, task: Task);
    fn pop_next(&mut self) -> Option<Task>;
    fn would_preempt_for(&mut self, key: TaskKey) -> bool;
    fn charge_running(&mut self, ns: u64);
    fn put_back(&mut self, task: Task, yielded: bool);
    fn leave_running(&mut self, task: &mut Task);
    fn pop_surplus(&mut self, loaded: Option<TaskKey>, asking: u64, thief: CpuId) -> Option<Task>;
    fn remove(&mut self, key: TaskKey) -> Option<Task>;
    fn refresh(&mut self);
    fn view(&self, now: Nanos, since: Nanos) -> View;
}

macro_rules! band {
    ($queue:ty) => {
        impl Band for $queue {
            fn insert(&mut self, task: Task) {
                <$queue>::insert(self, task)
            }
            fn pop_next(&mut self) -> Option<Task> {
                <$queue>::pop_next(self)
            }
            fn would_preempt_for(&mut self, key: TaskKey) -> bool {
                <$queue>::would_preempt_for(self, key)
            }
            fn charge_running(&mut self, ns: u64) {
                <$queue>::charge_running(self, ns)
            }
            fn put_back(&mut self, task: Task, yielded: bool) {
                <$queue>::put_back(self, task, yielded)
            }
            fn leave_running(&mut self, task: &mut Task) {
                <$queue>::leave_running(self, task.key(), task.standing_mut())
            }
            fn pop_surplus(&mut self, loaded: Option<TaskKey>, asking: u64, thief: CpuId) -> Option<Task> {
                <$queue>::pop_surplus(self, loaded, asking, thief)
            }
            fn remove(&mut self, key: TaskKey) -> Option<Task> {
                <$queue>::remove(self, key)
            }
            fn refresh(&mut self) {
                <$queue>::refresh(self)
            }
            fn view(&self, now: Nanos, since: Nanos) -> View {
                let keys: Vec<TaskKey> = self.keys().collect();
                View {
                    len: self.len(),
                    fair_len: self.fair_len(),
                    movable_fair: self.movable_fair(),
                    takers: self.takers(),
                    weight: self.weight(),
                    groups: self.groups().collect(),
                    movable: keys.iter().copied().filter(|&key| self.holds_movable(key)).collect(),
                    keys,
                    has_rt: self.has_rt(),
                    running_is_fair: self.running_is_fair(),
                    decision: self.decision(now, since),
                }
            }
        }
    };
}

band!(RunQueue<Payload>);
band!(reference::RunQueue<Payload>);

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

/// One side's queues, the task each CPU has popped, and the tasks in no
/// queue, each by its index.
struct World<Q> {
    cpus: Vec<Q>,
    running: Vec<Option<Task>>,
    out: Vec<Option<Task>>,
}

/// A task's description, the same on both sides.
struct Spec {
    job: usize,
    rt: bool,
    mask: CpuMask,
}

impl<Q: Band> World<Q> {
    fn new(cpus: usize, specs: &[Spec], new: impl Fn() -> Q) -> Self {
        let jobs: Vec<Arc<Job>> = (0..=specs.iter().map(|s| s.job).max().unwrap_or(0))
            .map(|j| Arc::new(Job::new(JobKey(j as u64 + 1))))
            .collect();
        let out = specs
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let builder = TaskBuilder {
                    key: TaskKey(i as u64 + 1),
                    job: jobs[spec.job].clone(),
                    ctx: (),
                    ext: Payload,
                    rt: RtState { permanent: spec.rt, inherited: None, lends: 0 },
                    mask: spec.mask,
                };
                Some(builder.build(CpuId(0), Nanos(0)).adopt(CpuId(0), Nanos(0)))
            })
            .collect();
        Self { cpus: (0..cpus).map(|_| new()).collect(), running: (0..cpus).map(|_| None).collect(), out }
    }

    fn index(key: TaskKey) -> usize {
        key.0 as usize - 1
    }

    fn put_out(&mut self, task: Task) {
        let i = Self::index(task.key());
        assert!(self.out[i].replace(task).is_none(), "a task out twice");
    }

    /// The standing the task at `i` left its last queue with, while it is out.
    fn standing(&mut self, i: usize) -> (Option<Left>, Lag) {
        let standing = self.out[i].as_mut().expect("out").standing_mut();
        (standing.left, standing.group_lag)
    }

    /// Abandon every task and queue: a task's only legal death is
    /// `DeadTask::finalize`, which a run queue never reaches.
    fn abandon(self) {
        #[expect(clippy::disallowed_methods, reason = "a live task dies only by `DeadTask::finalize`, never by a drop")]
        core::mem::forget(self);
    }
}

/// One generated operation, applied to both sides; each answer compared.
fn step<A: Band, B: Band>(
    rng: &mut Lcg,
    a: &mut World<A>,
    b: &mut World<B>,
    now: &mut Nanos,
    since: &mut [Nanos],
    masks: &[CpuMask],
) {
    let cpus = a.cpus.len();
    let c = rng.below(cpus as u64) as usize;
    let outs: Vec<usize> = (0..a.out.len()).filter(|&i| a.out[i].is_some()).collect();
    match rng.below(12) {
        0..=2 if !outs.is_empty() => {
            let i = outs[rng.below(outs.len() as u64) as usize];
            let key = TaskKey(i as u64 + 1);
            a.cpus[c].insert(a.out[i].take().unwrap());
            b.cpus[c].insert(b.out[i].take().unwrap());
            if a.running[c].is_some() {
                assert_eq!(a.cpus[c].would_preempt_for(key), b.cpus[c].would_preempt_for(key), "would_preempt_for {key:?}");
            }
        }
        3 | 4 if a.running[c].is_none() => {
            let (x, y) = (a.cpus[c].pop_next(), b.cpus[c].pop_next());
            assert_eq!(x.as_ref().map(|t| t.key()), y.as_ref().map(|t| t.key()), "pop_next on cpu{c}");
            a.running[c] = x;
            b.running[c] = y;
            since[c] = *now;
        }
        5 | 6 if a.running[c].is_some() => {
            let ns = 1 + rng.below(6_000_000);
            *now = now.after(ns);
            a.cpus[c].charge_running(ns);
            b.cpus[c].charge_running(ns);
        }
        7 if a.running[c].is_some() => {
            let yielded = rng.below(2) == 0;
            a.cpus[c].put_back(a.running[c].take().unwrap(), yielded);
            b.cpus[c].put_back(b.running[c].take().unwrap(), yielded);
        }
        8 if a.running[c].is_some() => {
            let (mut x, mut y) = (a.running[c].take().unwrap(), b.running[c].take().unwrap());
            a.cpus[c].leave_running(&mut x);
            b.cpus[c].leave_running(&mut y);
            let i = World::<A>::index(x.key());
            a.put_out(x);
            b.put_out(y);
            assert_eq!(a.standing(i), b.standing(i), "standing after leave_running");
        }
        9 => {
            let loaded = a.running[c].as_ref().map(|t| t.key());
            let asking = [0, JOB_WEIGHT / 3, JOB_WEIGHT / 2, JOB_WEIGHT][rng.below(4) as usize];
            let thief = CpuId(rng.below(cpus as u64) as u32);
            let (x, y) = (a.cpus[c].pop_surplus(loaded, asking, thief), b.cpus[c].pop_surplus(loaded, asking, thief));
            assert_eq!(x.as_ref().map(|t| t.key()), y.as_ref().map(|t| t.key()), "pop_surplus");
            if let (Some(x), Some(y)) = (x, y) {
                let i = World::<A>::index(x.key());
                a.put_out(x);
                b.put_out(y);
                assert_eq!(a.standing(i), b.standing(i), "standing after pop_surplus");
            }
        }
        10 => {
            let key = TaskKey(1 + rng.below(a.out.len() as u64));
            let (x, y) = (a.cpus[c].remove(key), b.cpus[c].remove(key));
            assert_eq!(x.as_ref().map(|t| t.key()), y.as_ref().map(|t| t.key()), "remove {key:?}");
            if let (Some(x), Some(y)) = (x, y) {
                a.put_out(x);
                b.put_out(y);
                let i = World::<A>::index(key);
                assert_eq!(a.standing(i), b.standing(i), "standing after remove");
            }
        }
        11 if !outs.is_empty() => {
            // A mask moves while its task is in no queue.
            let i = outs[rng.below(outs.len() as u64) as usize];
            let mask = masks[rng.below(masks.len() as u64) as usize];
            for task in [a.out[i].as_mut().unwrap(), b.out[i].as_mut().unwrap()] {
                task.shared().mask().store(mask, &NoPreempt);
                assert!(task.refresh_affinity());
            }
        }
        _ => {
            a.cpus[c].refresh();
            b.cpus[c].refresh();
        }
    }
    for (c, ((x, y), &since)) in a.cpus.iter().zip(&b.cpus).zip(since.iter()).enumerate() {
        assert_eq!(x.view(*now, since), y.view(*now, since), "cpu{c}");
    }
}

#[test]
fn the_run_queue_decides_as_the_one_it_replaced() {
    for seed in 0..300 {
        let mut rng = Lcg(seed);
        let cpus = 1 + rng.below(3) as usize;
        let mut masks = Vec::from([CpuMask::ANY]);
        masks.extend((0..cpus as u32).map(|c| CpuMask::one(CpuId(c))));
        if cpus > 1 {
            masks.push([CpuId(0), CpuId(1)].into_iter().collect());
        }
        let jobs = 1 + rng.below(4) as usize;
        let specs: Vec<Spec> = (0..2 + rng.below(10))
            .map(|_| Spec {
                job: rng.below(jobs as u64) as usize,
                rt: rng.below(8) == 0,
                mask: masks[rng.below(masks.len() as u64) as usize],
            })
            .collect();
        let mut a = World::new(cpus, &specs, RunQueue::<Payload>::new);
        let mut b = World::new(cpus, &specs, reference::RunQueue::<Payload>::new);
        let mut now = Nanos(1_000);
        let mut since = Vec::from_iter((0..cpus).map(|_| now));
        for _ in 0..2_000 {
            step(&mut rng, &mut a, &mut b, &mut now, &mut since, &masks);
        }
        a.abandon();
        b.abandon();
    }
}
