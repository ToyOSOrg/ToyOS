//! Job-fair EEVDF with one realtime class, over per-CPU queues: the host model
//! of the scheduling policy, driven one operation at a time by its caller.
//!
//! Ordinary work: each CPU runs EEVDF over the job groups it holds, and EEVDF
//! again over each group's threads ([`Eevdf`] at both levels). A job's weight is
//! divided among its groups by their share of its runnable threads, so its
//! groups' weights always sum to its own weight and a helper buys no
//! entitlement. Lag goes with whatever leaves a queue: a thread's to its next
//! queue, a dissolving group's to the job's next group.
//!
//! Realtime work: an authorized worker runs ahead of ordinary work on a CPU
//! while that CPU's [`Allowance`] lasts, round-robin with the CPU's other
//! workers. Exhaustion moves every worker on the CPU into its job's group until
//! that interval's boundary, wherever it goes meanwhile; that time is ordinary
//! and charged as ordinary.
//!
//! Every runnable thread is in exactly one queue — one group's or one CPU's
//! realtime queue — and the running thread stays in its queue.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::sched::eevdf::{Eevdf, Lag, Left};
use crate::sched::hw::{CpuId, Nanos};
use crate::sched::rt::{Allowance, RtOwner};

const MS: u64 = 1_000_000;
const US: u64 = 1_000;

/// One unit of job weight, as the fixed point every weight is expressed in.
pub const WEIGHT_UNIT: u64 = 1 << 16;
/// The largest job weight: 256 units, so a group weight (a job weight times a
/// 16-bit fraction) stays within [`crate::sched::eevdf::MAX_WEIGHT`].
pub const MAX_JOB_WEIGHT: u64 = 256 * WEIGHT_UNIT;

/// The engineering parameters, all in ns.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Request size of a thread that asks for none, and the longest any may ask.
    pub slice: u64,
    /// The shortest request a latency-sensitive thread may ask for, and the
    /// least a thread runs before an ordinary wake may preempt it.
    pub min_slice: u64,
    /// The most lag a thread or job keeps while out of every queue.
    pub lag_bound: u64,
    pub rt_interval: u64,
    pub rt_budget: u64,
    /// Round-robin turn between realtime workers on one CPU.
    pub rt_quantum: u64,
    /// Realtime execution without blocking that marks a runaway.
    pub runaway: u64,
    pub strikes_to_revoke: u32,
}

impl Params {
    pub const DEFAULT: Params = Params {
        slice: 4 * MS,
        min_slice: 250 * US,
        lag_bound: 4 * MS,
        rt_interval: 5 * MS,
        rt_budget: 3_500 * US,
        rt_quantum: MS,
        runaway: 50 * MS,
        strikes_to_revoke: 3,
    };
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct JobId {
    index: u32,
    generation: u32,
}

/// A thread's identity. The generation makes a reused slot a different thread,
/// so nothing granted to the old one reaches the new one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct ThreadId {
    index: u32,
    generation: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Band {
    Fair,
    Rt,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Blocked,
    Queued { cpu: CpuId, band: Band },
}

/// Why a realtime grant was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The thread has exited; its identity names no live thread.
    Gone,
    /// The thread's owner was revoked for runaway workers.
    OwnerRevoked,
}

/// What a wake obliges the CPU to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resched {
    No,
    Now,
    /// The woken thread wins, once the running one has had its minimum run.
    At(Nanos),
}

struct Job {
    weight: u64,
    refs: u32,
    /// Runnable ordinary threads on every CPU.
    runnable: u32,
    /// Lag of the job's last dissolved group, for its next one.
    carried: Lag,
    rt: RtOwner,
}

struct Thread {
    job: JobId,
    state: State,
    slice: u64,
    /// What the thread took from its last ordinary queue.
    left: Option<Left>,
    /// A realtime grant is held.
    granted: bool,
    /// No realtime treatment before this instant: set by an exhausted CPU to
    /// its boundary. The thread's own, so neither migration nor a new grant
    /// clears it.
    demoted_until: Nanos,
    /// Realtime execution since the thread last blocked; a new grant does not
    /// clear it either.
    unblocked_rt: u64,
    fair_ns: u64,
    rt_ns: u64,
}

struct Group {
    job: JobId,
    threads: Eevdf<ThreadId>,
}

struct Running {
    tid: ThreadId,
    band: Band,
    /// When it was dispatched.
    since: Nanos,
    /// Start of its current realtime round-robin turn.
    turn: Nanos,
}

struct Cpu {
    groups: Eevdf<JobId>,
    members: Vec<Group>,
    rt: VecDeque<ThreadId>,
    allowance: Allowance,
    current: Option<Running>,
    charged: Nanos,
    preempt_at: Option<Nanos>,
}

pub struct JobSched {
    params: Params,
    jobs: Vec<(u32, Option<Job>)>,
    threads: Vec<(u32, Option<Thread>)>,
    cpus: Vec<Cpu>,
}

impl JobSched {
    pub fn new(cpus: usize, params: Params, now: Nanos) -> Self {
        assert!(params.min_slice > 0 && params.min_slice <= params.slice);
        let cpus = (0..cpus)
            .map(|_| Cpu {
                groups: Eevdf::new(),
                members: Vec::new(),
                rt: VecDeque::new(),
                allowance: Allowance::new(params.rt_budget, params.rt_interval, now),
                current: None,
                charged: now,
                preempt_at: None,
            })
            .collect();
        Self { params, jobs: Vec::new(), threads: Vec::new(), cpus }
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    // ---- Ownership -----------------------------------------------------------

    /// A new job owner, held by one reference: its creator's.
    pub fn create_job(&mut self, weight: u64) -> JobId {
        assert!((1..=MAX_JOB_WEIGHT).contains(&weight), "job weight {weight} out of range");
        let job = Job { weight, refs: 1, runnable: 0, carried: Lag::ZERO, rt: RtOwner::default() };
        let (index, generation) = match self.jobs.iter().position(|(_, j)| j.is_none()) {
            Some(i) => {
                self.jobs[i].0 += 1;
                self.jobs[i].1 = Some(job);
                (i, self.jobs[i].0)
            }
            None => {
                self.jobs.push((0, Some(job)));
                (self.jobs.len() - 1, 0)
            }
        };
        JobId { index: index as u32, generation }
    }

    fn job(&self, id: JobId) -> &Job {
        match &self.jobs[id.index as usize] {
            (g, Some(job)) if *g == id.generation => job,
            _ => panic!("job {id:?} is not alive"),
        }
    }

    fn job_mut(&mut self, id: JobId) -> &mut Job {
        match &mut self.jobs[id.index as usize] {
            (g, Some(job)) if *g == id.generation => job,
            _ => panic!("job {id:?} is not alive"),
        }
    }

    pub fn job_alive(&self, id: JobId) -> bool {
        matches!(&self.jobs.get(id.index as usize), Some((g, Some(_))) if *g == id.generation)
    }

    pub fn retain_job(&mut self, id: JobId) {
        self.job_mut(id).refs += 1;
    }

    /// Drop one reference; the owner ends with its last.
    pub fn release_job(&mut self, id: JobId) {
        let job = self.job_mut(id);
        job.refs -= 1;
        if job.refs == 0 {
            assert_eq!(job.runnable, 0, "a job with runnable threads lost its last reference");
            self.jobs[id.index as usize].1 = None;
        }
    }

    /// Takes effect on each CPU at its next decision.
    pub fn set_weight(&mut self, id: JobId, weight: u64) {
        assert!((1..=MAX_JOB_WEIGHT).contains(&weight), "job weight {weight} out of range");
        self.job_mut(id).weight = weight;
    }

    pub fn rt_owner(&self, id: JobId) -> RtOwner {
        self.job(id).rt
    }

    /// A blocked thread of `job`, which it holds a reference to.
    pub fn spawn(&mut self, job: JobId) -> ThreadId {
        self.retain_job(job);
        let thread = Thread {
            job,
            state: State::Blocked,
            slice: self.params.slice,
            left: None,
            granted: false,
            demoted_until: Nanos::ZERO,
            unblocked_rt: 0,
            fair_ns: 0,
            rt_ns: 0,
        };
        let (index, generation) = match self.threads.iter().position(|(_, t)| t.is_none()) {
            Some(i) => {
                self.threads[i].0 += 1;
                self.threads[i].1 = Some(thread);
                (i, self.threads[i].0)
            }
            None => {
                self.threads.push((0, Some(thread)));
                (self.threads.len() - 1, 0)
            }
        };
        ThreadId { index: index as u32, generation }
    }

    fn thread(&self, id: ThreadId) -> &Thread {
        self.try_thread(id).unwrap_or_else(|| panic!("thread {id:?} is not alive"))
    }

    fn try_thread(&self, id: ThreadId) -> Option<&Thread> {
        match self.threads.get(id.index as usize) {
            Some((g, Some(t))) if *g == id.generation => Some(t),
            _ => None,
        }
    }

    fn thread_mut(&mut self, id: ThreadId) -> &mut Thread {
        match &mut self.threads[id.index as usize] {
            (g, Some(t)) if *g == id.generation => t,
            _ => panic!("thread {id:?} is not alive"),
        }
    }

    pub fn state(&self, id: ThreadId) -> State {
        self.thread(id).state
    }

    pub fn job_of(&self, id: ThreadId) -> JobId {
        self.thread(id).job
    }

    /// Ordinary and realtime CPU time the thread has received.
    pub fn service(&self, id: ThreadId) -> (u64, u64) {
        let t = self.thread(id);
        (t.fair_ns, t.rt_ns)
    }

    /// A blocked thread ends: its grant dies with it and its job loses a
    /// reference.
    pub fn exit(&mut self, id: ThreadId) {
        assert_eq!(self.thread(id).state, State::Blocked, "only a blocked thread exits");
        let job = self.thread(id).job;
        self.threads[id.index as usize].1 = None;
        self.release_job(job);
    }

    // ---- Realtime authority -------------------------------------------------

    /// Make `id` a realtime worker. The caller holds the authority to grant;
    /// the identity it names may be stale, which is refused.
    pub fn grant_rt(&mut self, id: ThreadId, now: Nanos) -> Result<(), Refused> {
        let job = self.try_thread(id).ok_or(Refused::Gone)?.job;
        if self.job(job).rt.revoked {
            return Err(Refused::OwnerRevoked);
        }
        if self.thread(id).granted {
            return Ok(());
        }
        self.thread_mut(id).granted = true;
        if let State::Queued { cpu, band: Band::Fair } = self.thread(id).state {
            self.charge(cpu, now);
            if self.rt_eligible(id, cpu, now) {
                self.fair_to_rt(cpu, id);
            }
        }
        Ok(())
    }

    /// End `id`'s realtime treatment (stream closed, granting service gone).
    pub fn revoke_rt(&mut self, id: ThreadId, now: Nanos) {
        if !self.thread(id).granted {
            return;
        }
        if let State::Queued { cpu, band: Band::Rt } = self.thread(id).state {
            self.charge(cpu, now);
            self.thread_mut(id).granted = false;
            self.rt_to_fair(cpu, id);
        } else {
            self.thread_mut(id).granted = false;
        }
    }

    /// Move a queued worker between bands on one CPU; a running one keeps
    /// running in its new band.
    fn rt_to_fair(&mut self, cpu: CpuId, id: ThreadId) {
        self.rt_leave(cpu, id);
        self.fair_enter(cpu, id);
        self.rebind_current(cpu, id, Band::Fair);
    }

    fn fair_to_rt(&mut self, cpu: CpuId, id: ThreadId) {
        self.fair_leave(cpu, id);
        self.rt_enter(cpu, id);
        self.rebind_current(cpu, id, Band::Rt);
    }

    fn rebind_current(&mut self, cpu: CpuId, id: ThreadId, band: Band) {
        if let Some(run) = &mut self.cpus[cpu.0 as usize].current {
            if run.tid == id {
                run.band = band;
            }
        }
    }

    fn rt_eligible(&self, id: ThreadId, cpu: CpuId, now: Nanos) -> bool {
        let t = self.thread(id);
        t.granted && t.demoted_until <= now && !self.cpus[cpu.0 as usize].allowance.exhausted()
    }

    // ---- Queue membership ---------------------------------------------------

    fn group_index(&self, cpu: CpuId, job: JobId) -> Option<usize> {
        self.cpus[cpu.0 as usize].members.iter().position(|g| g.job == job)
    }

    /// The weight `job`'s group on `cpu` has, by its share of the job's runnable
    /// threads.
    fn group_weight(&self, cpu: CpuId, job: JobId) -> u64 {
        let i = self.group_index(cpu, job).expect("the group exists");
        let here = self.cpus[cpu.0 as usize].members[i].threads.len() as u128;
        let all = self.job(job).runnable as u128;
        let w = (self.job(job).weight as u128 * here / all) as u64;
        w.max(1)
    }

    fn group_slice(&self, cpu: CpuId, i: usize) -> u64 {
        let group = &self.cpus[cpu.0 as usize].members[i];
        group.threads.keys().map(|t| self.thread(t).slice).min().expect("a group has a thread")
    }

    fn fair_enter(&mut self, cpu: CpuId, id: ThreadId) {
        let job = self.thread(id).job;
        let slice = self.thread(id).slice;
        self.job_mut(job).runnable += 1;
        let c = cpu.0 as usize;
        let i = match self.group_index(cpu, job) {
            Some(i) => i,
            None => {
                let carried = core::mem::take(&mut self.job_mut(job).carried);
                self.cpus[c].members.push(Group { job, threads: Eevdf::new() });
                // Weight is set below, once the thread is counted here.
                self.cpus[c].groups.join(job, 1, slice, carried);
                self.cpus[c].members.len() - 1
            }
        };
        let left = self.thread_mut(id).left.take();
        let threads = &mut self.cpus[c].members[i].threads;
        match left {
            Some(left) => threads.rejoin(id, WEIGHT_UNIT, slice, left),
            None => threads.join(id, WEIGHT_UNIT, slice, Lag::ZERO),
        };
        self.thread_mut(id).state = State::Queued { cpu, band: Band::Fair };
        let slice = self.group_slice(cpu, i);
        self.cpus[c].groups.set_slice(job, slice);
        self.after_runnable_change(cpu, job);
    }

    fn fair_leave(&mut self, cpu: CpuId, id: ThreadId) {
        let job = self.thread(id).job;
        let c = cpu.0 as usize;
        let i = self.group_index(cpu, job).expect("a queued thread's group exists");
        let mut left = self.cpus[c].members[i].threads.leave(id);
        left.lag = left.lag.clamp_ns(self.params.lag_bound);
        self.thread_mut(id).left = Some(left);
        self.job_mut(job).runnable -= 1;
        if self.cpus[c].members[i].threads.is_empty() {
            self.cpus[c].members.swap_remove(i);
            let lag = self.cpus[c].groups.leave(job).lag;
            let bound = self.params.lag_bound;
            let j = self.job_mut(job);
            j.carried = j.carried.saturating_add(lag).clamp_ns(bound);
        } else {
            let slice = self.group_slice(cpu, i);
            self.cpus[c].groups.set_slice(job, slice);
        }
        self.thread_mut(id).state = State::Blocked;
        self.after_runnable_change(cpu, job);
    }

    /// The CPU whose queue changed holds its own group's weight current; the
    /// job's other groups take the change up at their CPUs' own decisions.
    fn after_runnable_change(&mut self, cpu: CpuId, job: JobId) {
        if self.group_index(cpu, job).is_some() {
            let w = self.group_weight(cpu, job);
            self.cpus[cpu.0 as usize].groups.reweight(job, w);
        }
    }

    fn refresh_cpu(&mut self, cpu: CpuId) {
        let c = cpu.0 as usize;
        let jobs: Vec<JobId> = self.cpus[c].members.iter().map(|g| g.job).collect();
        for job in jobs {
            let w = self.group_weight(cpu, job);
            self.cpus[c].groups.reweight(job, w);
        }
    }

    fn rt_enter(&mut self, cpu: CpuId, id: ThreadId) {
        self.cpus[cpu.0 as usize].rt.push_back(id);
        self.thread_mut(id).state = State::Queued { cpu, band: Band::Rt };
    }

    fn rt_leave(&mut self, cpu: CpuId, id: ThreadId) {
        let rt = &mut self.cpus[cpu.0 as usize].rt;
        let i = rt.iter().position(|&t| t == id).expect("a realtime-queued thread is in its queue");
        rt.remove(i);
        self.thread_mut(id).state = State::Blocked;
    }

    /// Enter `cpu`'s queues in whichever band the thread is due, demoting a
    /// worker an exhausted CPU will not run as realtime.
    fn enter(&mut self, cpu: CpuId, id: ThreadId, now: Nanos) {
        self.cpus[cpu.0 as usize].allowance.observe(now);
        if self.rt_eligible(id, cpu, now) {
            self.rt_enter(cpu, id);
            return;
        }
        let boundary = self.cpus[cpu.0 as usize].allowance.boundary();
        let exhausted = self.cpus[cpu.0 as usize].allowance.exhausted();
        let t = self.thread_mut(id);
        if t.granted && exhausted {
            t.demoted_until = t.demoted_until.max(boundary);
        }
        self.fair_enter(cpu, id);
    }

    fn leave(&mut self, cpu: CpuId, id: ThreadId) {
        match self.thread(id).state {
            State::Queued { band: Band::Fair, .. } => self.fair_leave(cpu, id),
            State::Queued { band: Band::Rt, .. } => self.rt_leave(cpu, id),
            State::Blocked => unreachable!("leaving a queue it is not in"),
        }
    }

    // ---- Events ---------------------------------------------------------------

    /// Charge `cpu`'s running thread for the time since it was last charged.
    pub fn charge(&mut self, cpu: CpuId, now: Nanos) {
        let c = cpu.0 as usize;
        let from = self.cpus[c].charged;
        assert!(now >= from, "time went backwards on {cpu:?}");
        self.cpus[c].charged = now;
        let Some(run) = &self.cpus[c].current else { return };
        let (tid, band) = (run.tid, run.band);
        let ns = now.since(from);
        if ns == 0 {
            return;
        }
        let job = self.thread(tid).job;
        match band {
            Band::Fair => {
                let i = self.group_index(cpu, job).expect("the running thread's group");
                self.cpus[c].groups.charge(job, ns);
                self.cpus[c].members[i].threads.charge(tid, ns);
                self.thread_mut(tid).fair_ns += ns;
            }
            Band::Rt => {
                self.cpus[c].allowance.charge(from, now);
                let runaway = self.params.runaway;
                let t = self.thread_mut(tid);
                t.rt_ns += ns;
                assert!(t.granted, "a realtime thread holds a grant");
                t.unblocked_rt += ns;
                let ran_away = t.unblocked_rt >= runaway;
                if ran_away {
                    t.unblocked_rt -= runaway;
                }
                let strikes = self.params.strikes_to_revoke;
                if ran_away && self.job_mut(job).rt.strike(strikes) {
                    self.revoke_owner(job, now);
                }
            }
        }
    }

    fn revoke_owner(&mut self, job: JobId, now: Nanos) {
        let workers: Vec<ThreadId> = (0..self.threads.len())
            .filter_map(|i| match &self.threads[i] {
                (g, Some(t)) if t.job == job && t.granted => {
                    Some(ThreadId { index: i as u32, generation: *g })
                }
                _ => None,
            })
            .collect();
        for id in workers {
            if let State::Queued { cpu, band: Band::Rt } = self.thread(id).state {
                self.charge(cpu, now);
                self.thread_mut(id).granted = false;
                self.rt_to_fair(cpu, id);
            } else {
                self.thread_mut(id).granted = false;
            }
        }
    }

    /// A blocked thread becomes runnable on `cpu`.
    pub fn wake(&mut self, id: ThreadId, cpu: CpuId, now: Nanos) -> Resched {
        assert_eq!(self.thread(id).state, State::Blocked, "waking a thread that is not blocked");
        self.charge(cpu, now);
        self.enter(cpu, id, now);
        self.wake_preempts(cpu, id, now)
    }

    fn wake_preempts(&mut self, cpu: CpuId, id: ThreadId, now: Nanos) -> Resched {
        let c = cpu.0 as usize;
        let Some(run) = &self.cpus[c].current else { return Resched::Now };
        let (since, current_band) = (run.since, run.band);
        if current_band == Band::Rt {
            return Resched::No;
        }
        if self.thread(id).state == (State::Queued { cpu, band: Band::Rt }) {
            return Resched::Now;
        }
        if self.refresh_and_pick_fair(cpu) != Some(id) {
            return Resched::No;
        }
        let at = since.after(self.params.min_slice);
        if at <= now {
            return Resched::Now;
        }
        let c = &mut self.cpus[c];
        c.preempt_at = Some(c.preempt_at.map_or(at, |p| p.min(at)));
        Resched::At(at)
    }

    /// The running thread on `cpu` blocks.
    pub fn block(&mut self, cpu: CpuId, now: Nanos) -> ThreadId {
        self.charge(cpu, now);
        let c = cpu.0 as usize;
        let run = self.cpus[c].current.take().expect("block with nothing running");
        self.leave(cpu, run.tid);
        self.thread_mut(run.tid).unblocked_rt = 0;
        run.tid
    }

    /// Move a queued, not running, thread to another CPU's queues.
    pub fn migrate(&mut self, id: ThreadId, to: CpuId, now: Nanos) {
        let State::Queued { cpu: from, .. } = self.thread(id).state else {
            panic!("migrating a thread that is not queued");
        };
        assert!(
            self.cpus[from.0 as usize].current.as_ref().is_none_or(|r| r.tid != id),
            "migrating the running thread",
        );
        self.charge(from, now);
        self.charge(to, now);
        self.leave(from, id);
        self.enter(to, id, now);
    }

    pub fn set_slice(&mut self, id: ThreadId, slice: u64, now: Nanos) {
        let slice = slice.clamp(self.params.min_slice, self.params.slice);
        self.thread_mut(id).slice = slice;
        if let State::Queued { cpu, band: Band::Fair } = self.thread(id).state {
            self.charge(cpu, now);
            let job = self.thread(id).job;
            let c = cpu.0 as usize;
            let i = self.group_index(cpu, job).unwrap();
            self.cpus[c].members[i].threads.set_slice(id, slice);
            let gs = self.group_slice(cpu, i);
            self.cpus[c].groups.set_slice(job, gs);
        }
    }

    fn refresh_and_pick_fair(&mut self, cpu: CpuId) -> Option<ThreadId> {
        self.refresh_cpu(cpu);
        let c = &self.cpus[cpu.0 as usize];
        let job = c.groups.pick()?;
        let group = c.members.iter().find(|g| g.job == job).expect("a queued group has members");
        Some(group.threads.pick().expect("a group has a thread"))
    }

    /// Decide what `cpu` runs from `now`.
    pub fn schedule(&mut self, cpu: CpuId, now: Nanos) -> Option<ThreadId> {
        self.charge(cpu, now);
        let c = cpu.0 as usize;
        self.cpus[c].preempt_at = None;
        self.cpus[c].allowance.observe(now);
        if self.cpus[c].allowance.exhausted() {
            let boundary = self.cpus[c].allowance.boundary();
            while let Some(&id) = self.cpus[c].rt.front() {
                self.rt_leave(cpu, id);
                self.thread_mut(id).demoted_until = boundary;
                self.fair_enter(cpu, id);
            }
        } else {
            for id in self.demoted_here(cpu) {
                if self.rt_eligible(id, cpu, now) {
                    self.fair_leave(cpu, id);
                    self.rt_enter(cpu, id);
                }
            }
        }
        let previous = self.cpus[c].current.take();
        if let Some(run) = &previous {
            if run.band == Band::Rt
                && self.thread(run.tid).state == (State::Queued { cpu, band: Band::Rt })
                && now.since(run.turn) >= self.params.rt_quantum
            {
                let rt = &mut self.cpus[c].rt;
                let i = rt.iter().position(|&t| t == run.tid).unwrap();
                let id = rt.remove(i).unwrap();
                rt.push_back(id);
            }
        }
        let (tid, band) = match self.cpus[c].rt.front() {
            Some(&id) => (id, Band::Rt),
            None => (self.refresh_and_pick_fair(cpu)?, Band::Fair),
        };
        let (since, turn) = match previous {
            Some(run) if run.tid == tid && run.band == band => {
                let turn = if band == Band::Rt && now.since(run.turn) >= self.params.rt_quantum {
                    now
                } else {
                    run.turn
                };
                (run.since, turn)
            }
            _ => (now, now),
        };
        self.cpus[c].current = Some(Running { tid, band, since, turn });
        Some(tid)
    }

    /// Ordinary-band workers on `cpu` that hold a grant.
    fn demoted_here(&self, cpu: CpuId) -> Vec<ThreadId> {
        let c = &self.cpus[cpu.0 as usize];
        c.members
            .iter()
            .flat_map(|g| g.threads.keys())
            .filter(|&t| self.thread(t).granted)
            .collect()
    }

    pub fn current(&self, cpu: CpuId) -> Option<(ThreadId, Band)> {
        self.cpus[cpu.0 as usize].current.as_ref().map(|r| (r.tid, r.band))
    }

    /// The next instant `cpu` must be scheduled again absent other events: its
    /// running request's end, its realtime turn or allowance running out, a
    /// deferred wake preemption, or a demoted worker becoming due.
    pub fn next_decision(&self, cpu: CpuId) -> Option<Nanos> {
        let c = &self.cpus[cpu.0 as usize];
        let mut next: Option<Nanos> = c.preempt_at;
        let mut at = |t: Nanos| next = Some(next.map_or(t, |n| n.min(t)));
        if let Some(run) = &c.current {
            match run.band {
                Band::Fair => {
                    let job = self.thread(run.tid).job;
                    let group = c.members.iter().find(|g| g.job == job).unwrap();
                    let left = c.groups.request_left_ns(job).min(group.threads.request_left_ns(run.tid));
                    at(c.charged.after(left));
                }
                Band::Rt => {
                    at(c.charged.after(c.allowance.remaining().max(1)));
                    if c.rt.len() > 1 {
                        at(run.turn.after(self.params.rt_quantum));
                    }
                }
            }
        }
        for g in &c.members {
            for t in g.threads.keys() {
                let t = self.thread(t);
                if t.granted {
                    let due = if c.allowance.exhausted() {
                        t.demoted_until.max(c.allowance.boundary())
                    } else {
                        t.demoted_until
                    };
                    at(due.max(c.charged));
                }
            }
        }
        next
    }

    // ---- Inspection -----------------------------------------------------------

    /// The sum of `cpu`'s group weights: what balancing compares.
    pub fn load(&self, cpu: CpuId) -> u128 {
        self.cpus[cpu.0 as usize].groups.total_weight()
    }

    /// Runnable threads on `cpu` in either band, the running one included.
    pub fn queued(&self, cpu: CpuId) -> impl Iterator<Item = ThreadId> + '_ {
        let c = &self.cpus[cpu.0 as usize];
        c.rt.iter().copied().chain(c.members.iter().flat_map(|g| g.threads.keys()))
    }

    /// The weight one more runnable thread of `job` adds wherever it goes.
    pub fn thread_weight(&self, job: JobId) -> u64 {
        let j = self.job(job);
        (j.weight / u64::from(j.runnable.max(1))).max(1)
    }

    pub fn group_weight_on(&self, cpu: CpuId, job: JobId) -> Option<u64> {
        self.group_index(cpu, job).map(|_| self.cpus[cpu.0 as usize].groups.weight(job))
    }

    pub fn allowance(&self, cpu: CpuId) -> Allowance {
        self.cpus[cpu.0 as usize].allowance
    }

    /// Every structural invariant: each runnable thread in exactly the one
    /// queue its state names, counts matching queues, groups matching members.
    pub fn check(&self) {
        let mut seen = Vec::new();
        for (c, cpu) in self.cpus.iter().enumerate() {
            let here = CpuId(c as u32);
            assert_eq!(cpu.groups.len(), cpu.members.len(), "{here:?}: groups and members disagree");
            for g in &cpu.members {
                assert!(cpu.groups.contains(g.job), "{here:?}: a member group is not queued");
                assert!(!g.threads.is_empty(), "{here:?}: an empty group stayed queued");
                for t in g.threads.keys() {
                    assert_eq!(self.thread(t).job, g.job, "{t:?} queued in another job's group");
                    assert_eq!(self.thread(t).state, State::Queued { cpu: here, band: Band::Fair });
                    seen.push(t);
                }
            }
            for &t in &cpu.rt {
                assert_eq!(self.thread(t).state, State::Queued { cpu: here, band: Band::Rt });
                assert!(self.thread(t).granted, "{t:?} realtime-queued without a grant");
                seen.push(t);
            }
            if let Some(run) = &cpu.current {
                assert_eq!(self.thread(run.tid).state, State::Queued { cpu: here, band: run.band });
            }
        }
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n, "a thread is in two queues");
        for (i, (g, slot)) in self.threads.iter().enumerate() {
            if let Some(t) = slot {
                let id = ThreadId { index: i as u32, generation: *g };
                assert_eq!(seen.binary_search(&id).is_ok(), t.state != State::Blocked, "{id:?}");
            }
        }
        for (i, (g, slot)) in self.jobs.iter().enumerate() {
            if let Some(job) = slot {
                let id = JobId { index: i as u32, generation: *g };
                let runnable = seen.iter().filter(|&&t| {
                    self.thread(t).job == id && matches!(self.thread(t).state, State::Queued { band: Band::Fair, .. })
                });
                assert_eq!(runnable.count() as u32, job.runnable, "{id:?}'s runnable count");
            }
        }
    }
}

#[cfg(test)]
mod checkpoint;
#[cfg(test)]
mod sim;
#[cfg(test)]
mod tests;
