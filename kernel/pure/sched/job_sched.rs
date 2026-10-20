//! Job-fair EEVDF with one realtime class, over per-CPU queues: the host model
//! of the scheduling policy, driven one operation at a time by its caller.
//!
//! Ordinary work: each CPU runs EEVDF over the job groups it holds, and EEVDF
//! again over each group's threads ([`Eevdf`] at both levels). A job's weight is
//! divided among its groups by their share of its runnable threads, so a
//! helper buys no entitlement: once every CPU holding a group has refreshed it,
//! the groups' weights sum to the job's, less one rounding unit per group. Lag
//! goes with whatever leaves a queue: a thread's to its next queue, and a
//! dissolving group's with its last thread to the group that thread next
//! enters.
//!
//! Across CPUs, each job counts its ordinary service per unit of weight.
//! Each CPU orders its groups by that count against the jobs it holds, and
//! placement moves threads ([`JobSched::fairness_move`]) only between jobs that
//! share no CPU, once one leads the other by `move_lead`. Per-CPU lag alone
//! cannot share indivisible thread counts fairly across CPUs.
//!
//! Realtime work: an authorized worker runs ahead of ordinary work on a CPU
//! while that CPU's [`Allowance`] lasts, round-robin with the CPU's other
//! workers. Exhaustion moves every worker on the CPU to the spare band until
//! that interval's boundary, wherever it goes meanwhile (owner decision J2): a
//! spare worker runs only when no ordinary work on the CPU is runnable, and
//! ordinary work becoming runnable preempts it. Spare time is charged to the
//! thread alone — not to the allowance, not to any group and not to its job's
//! normalized service — because it is capacity no ordinary work there wanted.
//!
//! Every runnable thread is in exactly one queue — one group's, or one CPU's
//! realtime or spare queue — and the running thread stays in its queue.

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
/// The smallest job weight: 1/256 of a unit, so a job's normalized service
/// grows by at most 256 per ns and its `u64` wraps after 2^56 ns of the job's
/// CPU time. Comparisons are wrapping differences, sound while two compared
/// jobs are within 2^63 of each other — `lead_cap` keeps every compared pair
/// far closer.
pub const MIN_JOB_WEIGHT: u64 = WEIGHT_UNIT / 256;

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
    /// Cross-CPU fairness: order each CPU's groups by their jobs'
    /// normalized service, and let placement move threads between jobs that
    /// share no CPU ([`JobSched::fairness_move`]). Off is the plain candidate:
    /// per-CPU lag only.
    pub cross_cpu_lag: bool,
    /// How often each CPU looks for a [`JobSched::fairness_move`].
    pub fairness_period: u64,
    /// Normalized service a job must lead another by before placement moves
    /// CPU time from the first to the second.
    pub move_lead: u64,
    /// The most normalized service a job may trail another on the same CPU, or
    /// on a CPU it is compared with, by: the cross-CPU counterpart of
    /// `lag_bound`, and the longest a stale job can keep a CPU's other jobs
    /// waiting (per unit of their weight).
    pub lead_cap: u64,
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
        cross_cpu_lag: true,
        fairness_period: 20 * MS,
        move_lead: 32 * MS,
        lead_cap: 64 * MS,
    };

    /// The candidate the checkpoint started from: per-CPU lag only.
    pub const CANDIDATE: Params = Params { cross_cpu_lag: false, ..Params::DEFAULT };
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
    /// A demoted realtime worker: runs only on capacity ordinary work leaves.
    Spare,
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

/// Jobs with their normalized service, relative to one of them.
type Standing = Vec<(JobId, i64)>;

/// `a − b` for two normalized services, read across a wrap.
fn lead(a: u64, b: u64) -> i64 {
    a.wrapping_sub(b) as i64
}

/// A placement change: one thread moved, or two traded, as
/// `(thread, from, to)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    pub threads: [Option<(ThreadId, CpuId, CpuId)>; 2],
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
    /// Ordinary service divided by weight, in ns at one weight unit, wrapping:
    /// equal for every job the fluid share does not cap. Written at charges
    /// (with `served_rem`, the division's remainder) and by the `lead_cap`
    /// clamps.
    served: u64,
    served_rem: u64,
    /// While the job has no runnable thread, and until it next shares a CPU
    /// with another job: its normalized service relative to the jobs it last
    /// shared one with, clamped to `lag_bound`. Restored against the jobs it
    /// meets next, so a sleep earns no cross-CPU credit and costs none, as lag
    /// carries a sleeper within one queue. A new job starts level.
    standing: Option<i64>,
    /// The CPUs holding a group of the job, sorted.
    cpus: Vec<CpuId>,
    rt: RtOwner,
}

struct Thread {
    job: JobId,
    state: State,
    slice: u64,
    /// What the thread took from its last ordinary queue.
    left: Option<Left>,
    /// The lag of the group it was the last to leave, for the group it next
    /// enters: a group's standing against other jobs goes with its last thread.
    group_lag: Lag,
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
    spare_ns: u64,
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
    spare: VecDeque<ThreadId>,
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
                spare: VecDeque::new(),
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
        assert!((MIN_JOB_WEIGHT..=MAX_JOB_WEIGHT).contains(&weight), "job weight {weight} out of range");
        let job = Job {
            weight,
            refs: 1,
            runnable: 0,
            served: 0,
            served_rem: 0,
            standing: Some(0),
            cpus: Vec::new(),
            rt: RtOwner::default(),
        };
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
        assert!((MIN_JOB_WEIGHT..=MAX_JOB_WEIGHT).contains(&weight), "job weight {weight} out of range");
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
            group_lag: Lag::ZERO,
            granted: false,
            demoted_until: Nanos::ZERO,
            unblocked_rt: 0,
            fair_ns: 0,
            rt_ns: 0,
            spare_ns: 0,
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

    /// CPU time the thread has received in the spare band.
    pub fn spare_service(&self, id: ThreadId) -> u64 {
        self.thread(id).spare_ns
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
            self.leave(cpu, id);
            self.enter(cpu, id, now);
            let band = self.band_of(id);
            self.rebind_current(cpu, id, band);
        }
        Ok(())
    }

    /// End `id`'s realtime treatment (stream closed, granting service gone).
    /// An identity that names no live thread is refused, as a grant to it is.
    pub fn revoke_rt(&mut self, id: ThreadId, now: Nanos) -> Result<(), Refused> {
        self.try_thread(id).ok_or(Refused::Gone)?;
        if self.thread(id).granted {
            self.end_grant(id, now);
        }
        Ok(())
    }

    /// Clear the grant; a queued worker goes to its job's group.
    fn end_grant(&mut self, id: ThreadId, now: Nanos) {
        match self.thread(id).state {
            State::Queued { cpu, band: Band::Rt | Band::Spare } => {
                self.charge(cpu, now);
                self.thread_mut(id).granted = false;
                self.leave(cpu, id);
                self.fair_enter(cpu, id);
                self.rebind_current(cpu, id, Band::Fair);
            }
            _ => self.thread_mut(id).granted = false,
        }
    }

    fn band_of(&self, id: ThreadId) -> Band {
        match self.thread(id).state {
            State::Queued { band, .. } => band,
            State::Blocked => unreachable!("a queued thread"),
        }
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
        let carried = core::mem::take(&mut self.thread_mut(id).group_lag);
        let i = match self.group_index(cpu, job) {
            Some(i) => {
                if carried != Lag::ZERO {
                    let groups = &mut self.cpus[c].groups;
                    let (weight, gslice) = (groups.weight(job), groups.slice(job));
                    let mut left = groups.leave(job);
                    left.lag = left.lag.saturating_add(carried).clamp_ns(self.params.lag_bound);
                    groups.rejoin(job, weight, gslice, left);
                }
                i
            }
            None => {
                let cpus = &mut self.job_mut(job).cpus;
                let at = cpus.binary_search(&cpu).expect_err("no group yet on this CPU");
                cpus.insert(at, cpu);
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
            let lag = self.cpus[c].groups.leave(job).lag.clamp_ns(self.params.lag_bound);
            self.thread_mut(id).group_lag = lag;
            let cpus = &mut self.job_mut(job).cpus;
            let at = cpus.binary_search(&cpu).expect("the job's group was on this CPU");
            cpus.remove(at);
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
        for &job in &jobs {
            let w = self.group_weight(cpu, job);
            self.cpus[c].groups.reweight(job, w);
        }
        if self.params.cross_cpu_lag {
            self.order_by_service(cpu, &jobs);
        }
    }

    /// Order `cpu`'s groups by their jobs' normalized service. First, a job
    /// back from having no runnable thread (or new) is placed at its standing
    /// against the jobs here, and any job trailing the CPU's leader by more
    /// than `lead_cap` is raised to that distance. Then each group's lag
    /// target is `w·(mean − served)`
    /// clamped to `lag_bound`, applied together by [`Eevdf::assign_lags`]: a
    /// group whose target was clamped keeps it, less its share of whatever the
    /// clamping left unbalanced.
    fn order_by_service(&mut self, cpu: CpuId, jobs: &[JobId]) {
        if jobs.len() < 2 {
            return;
        }
        let c = cpu.0 as usize;
        self.restore_standings(jobs);
        let reference = self.job(jobs[0]).served;
        let leader = jobs.iter().map(|&j| lead(self.job(j).served, reference)).max().unwrap();
        let cap = self.params.lead_cap as i64;
        for &j in jobs {
            if leader - lead(self.job(j).served, reference) > cap {
                self.job_mut(j).served = reference.wrapping_add((leader - cap) as u64);
            }
        }
        let w: Vec<i128> = jobs.iter().map(|&j| self.cpus[c].groups.weight(j) as i128).collect();
        let served: Vec<i128> = jobs.iter().map(|&j| lead(self.job(j).served, reference) as i128).collect();
        let total: i128 = w.iter().sum();
        let mean = w.iter().zip(&served).map(|(w, s)| w * s).sum::<i128>() / total;
        let targets: Vec<(JobId, Lag)> = jobs
            .iter()
            .enumerate()
            .map(|(k, &job)| {
                let owed = ((mean - served[k]) * w[k]) << crate::sched::eevdf::VSHIFT;
                (job, Lag(owed / WEIGHT_UNIT as i128).clamp_ns(self.params.lag_bound))
            })
            .collect();
        self.cpus[c].groups.assign_lags(&targets);
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

    /// `job`'s normalized service against the mean of the other jobs on CPU
    /// `c`, clamped to `lag_bound`; `None` with no other job there.
    fn standing_on(&self, c: usize, job: JobId) -> Option<i64> {
        let others: Vec<u64> =
            self.cpus[c].members.iter().filter(|g| g.job != job).map(|g| self.job(g.job).served).collect();
        let &reference = others.first()?;
        let mean = others.iter().map(|&s| lead(s, reference)).sum::<i64>() / others.len() as i64;
        let bound = self.params.lag_bound as i64;
        Some((lead(self.job(job).served, reference) - mean).clamp(-bound, bound))
    }

    /// Place every job among `jobs` that holds a standing at that standing
    /// against the mean of those that do not — or, if all do, against the
    /// first of them — and clear it. A lone job keeps its standing until it
    /// meets another.
    fn restore_standings(&mut self, jobs: &[JobId]) {
        let pending: Vec<JobId> = jobs.iter().copied().filter(|&j| self.job(j).standing.is_some()).collect();
        if pending.is_empty() || jobs.len() < 2 {
            return;
        }
        let settled: Vec<JobId> = jobs.iter().copied().filter(|&j| self.job(j).standing.is_none()).collect();
        let (reference, mean) = match settled.first() {
            Some(&first) => {
                let r = self.job(first).served;
                (r, settled.iter().map(|&j| lead(self.job(j).served, r)).sum::<i64>() / settled.len() as i64)
            }
            None => (self.job(pending[0]).served, self.job(pending[0]).standing.unwrap()),
        };
        for j in pending {
            let standing = self.job_mut(j).standing.take().unwrap();
            self.job_mut(j).served = reference.wrapping_add((mean + standing) as u64);
        }
    }

    fn spare_enter(&mut self, cpu: CpuId, id: ThreadId) {
        self.cpus[cpu.0 as usize].spare.push_back(id);
        self.thread_mut(id).state = State::Queued { cpu, band: Band::Spare };
    }

    fn spare_leave(&mut self, cpu: CpuId, id: ThreadId) {
        let spare = &mut self.cpus[cpu.0 as usize].spare;
        let i = spare.iter().position(|&t| t == id).expect("a spare-queued thread is in its queue");
        spare.remove(i);
        self.thread_mut(id).state = State::Blocked;
    }

    /// Enter `cpu`'s queues in whichever band the thread is due: a worker the
    /// CPU will not run as realtime now goes to the spare band, and is demoted
    /// to the boundary of an exhausted CPU.
    fn enter(&mut self, cpu: CpuId, id: ThreadId, now: Nanos) {
        self.cpus[cpu.0 as usize].allowance.observe(now);
        if self.rt_eligible(id, cpu, now) {
            self.rt_enter(cpu, id);
            return;
        }
        if !self.thread(id).granted {
            self.fair_enter(cpu, id);
            return;
        }
        let allowance = self.cpus[cpu.0 as usize].allowance;
        if allowance.exhausted() {
            let t = self.thread_mut(id);
            t.demoted_until = t.demoted_until.max(allowance.boundary());
        }
        self.spare_enter(cpu, id);
    }

    fn leave(&mut self, cpu: CpuId, id: ThreadId) {
        match self.band_of(id) {
            Band::Fair => self.fair_leave(cpu, id),
            Band::Rt => self.rt_leave(cpu, id),
            Band::Spare => self.spare_leave(cpu, id),
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
                let j = self.job_mut(job);
                let scaled = ns as u128 * WEIGHT_UNIT as u128 + j.served_rem as u128;
                j.served = j.served.wrapping_add((scaled / j.weight as u128) as u64);
                j.served_rem = (scaled % j.weight as u128) as u64;
            }
            Band::Spare => self.thread_mut(tid).spare_ns += ns,
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
            self.end_grant(id, now);
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
        let woken = self.band_of(id);
        match (current_band, woken) {
            (Band::Rt, _) | (Band::Spare, Band::Spare) => return Resched::No,
            (Band::Spare, _) | (Band::Fair, Band::Rt) => return Resched::Now,
            (Band::Fair, Band::Spare) => return Resched::No,
            (Band::Fair, Band::Fair) => {}
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
        let job = self.thread(run.tid).job;
        let fair = self.band_of(run.tid) == Band::Fair;
        self.leave(cpu, run.tid);
        self.thread_mut(run.tid).unblocked_rt = 0;
        // The job's last runnable thread sleeps; a migration leaves and enters
        // without one.
        if fair && self.job(job).runnable == 0 && self.job(job).standing.is_none() {
            let standing = self.standing_on(c, job).unwrap_or(0);
            self.job_mut(job).standing = Some(standing);
        }
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
                self.spare_enter(cpu, id);
            }
        } else {
            for id in self.cpus[c].spare.clone() {
                if self.rt_eligible(id, cpu, now) {
                    self.spare_leave(cpu, id);
                    self.rt_enter(cpu, id);
                }
            }
        }
        let previous = self.cpus[c].current.take();
        if let Some(run) = &previous {
            let band = run.band;
            if band != Band::Fair
                && self.thread(run.tid).state == (State::Queued { cpu, band })
                && now.since(run.turn) >= self.params.rt_quantum
            {
                let queue = if band == Band::Rt { &mut self.cpus[c].rt } else { &mut self.cpus[c].spare };
                let i = queue.iter().position(|&t| t == run.tid).unwrap();
                let id = queue.remove(i).unwrap();
                queue.push_back(id);
            }
        }
        let (tid, band) = match self.cpus[c].rt.front() {
            Some(&id) => (id, Band::Rt),
            None => match self.refresh_and_pick_fair(cpu) {
                Some(id) => (id, Band::Fair),
                None => (*self.cpus[c].spare.front()?, Band::Spare),
            },
        };
        let (since, turn) = match previous {
            Some(run) if run.tid == tid && run.band == band => {
                let turn = if band != Band::Fair && now.since(run.turn) >= self.params.rt_quantum {
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
                    // Already exhausted by a charge another CPU's move made:
                    // decide at once, so no execution goes past the allowance.
                    at(c.charged.after(c.allowance.remaining()));
                    if c.rt.len() > 1 {
                        at(run.turn.after(self.params.rt_quantum));
                    }
                }
                Band::Spare => {
                    if c.spare.len() > 1 {
                        at(run.turn.after(self.params.rt_quantum));
                    }
                }
            }
        }
        for &t in &c.spare {
            let t = self.thread(t);
            let due = if c.allowance.exhausted() {
                t.demoted_until.max(c.allowance.boundary())
            } else {
                t.demoted_until
            };
            at(due.max(c.charged));
        }
        next
    }

    // ---- Cross-CPU fairness --------------------------------------------------

    /// Take `cpu`'s running thread off the CPU, leaving it queued, so that it
    /// can be moved; the caller decides again for `cpu`.
    pub fn preempt(&mut self, cpu: CpuId, now: Nanos) {
        self.charge(cpu, now);
        self.cpus[cpu.0 as usize].current = None;
    }

    /// The placement change `cpu`'s fairness step makes with one of
    /// `partners`, if any: one thread moved, or two threads traded.
    ///
    /// A change is judged by its exact effect on each job's ordinary rate on
    /// the two CPUs — group weight over CPU load, with the job's weight split
    /// by its runnable threads as the queues split it. It is allowed only
    /// between jobs that share no CPU (those that share one are ordered there
    /// by [`Self::order_by_service`]), and only if every job that loses rate
    /// leads the most-behind job that gains by more than `move_lead`. Of the
    /// allowed changes, the one with the largest such lead is made. A job whose
    /// threads all run gains nothing from any change and trails the jobs it
    /// could lose to, so nothing is taken from it. Loads are not an objective.
    ///
    /// Only counts per job enter the judgement, so candidates are one thread
    /// per group (a waiting one if the group has one): with `g` groups on the
    /// pair, `O(g²)` candidates, each judged in `O(g)`. Each pair first raises
    /// any of its jobs trailing the pair's leader by more than `lead_cap` to
    /// that distance. Reads: the pair's queues, and for each job on them its
    /// weight, runnable count, normalized service and set of CPUs.
    pub fn fairness_move(&mut self, cpu: CpuId, partners: &[CpuId]) -> Option<Move> {
        let mut best: Option<(i64, Move)> = None;
        for &other in partners {
            if other == cpu {
                continue;
            }
            self.clamp_trailing(cpu, other);
            for (gap, change) in self.changes(cpu, other) {
                if best.as_ref().is_none_or(|(g, _)| gap > *g) {
                    best = Some((gap, change));
                }
            }
        }
        best.map(|(_, change)| change)
    }

    /// One movable thread per ordinary group on `c`: a waiting one if any.
    fn representatives(&self, c: CpuId) -> Vec<ThreadId> {
        let running = self.current(c).map(|r| r.0);
        self.cpus[c.0 as usize]
            .members
            .iter()
            .filter_map(|g| {
                let mut threads = g.threads.keys().filter(|&t| !self.thread(t).granted);
                let first = threads.next()?;
                if Some(first) != running {
                    return Some(first);
                }
                Some(threads.next().unwrap_or(first))
            })
            .collect()
    }

    fn jobs_on(&self, a: CpuId, b: CpuId) -> Vec<JobId> {
        let mut jobs: Vec<JobId> = [a, b]
            .iter()
            .flat_map(|&c| self.cpus[c.0 as usize].members.iter().map(|g| g.job))
            .collect();
        jobs.sort_unstable();
        jobs.dedup();
        jobs
    }

    fn clamp_trailing(&mut self, a: CpuId, b: CpuId) {
        let jobs = self.jobs_on(a, b);
        let Some(&first) = jobs.first() else { return };
        self.restore_standings(&jobs);
        let reference = self.job(first).served;
        let leader = jobs.iter().map(|&j| lead(self.job(j).served, reference)).max().unwrap();
        let cap = self.params.lead_cap as i64;
        for j in jobs {
            if leader - lead(self.job(j).served, reference) > cap {
                self.job_mut(j).served = reference.wrapping_add((leader - cap) as u64);
            }
        }
    }

    /// Each job's ordinary rate on `a` plus `b`, in CPUs scaled by 2^32, with
    /// `count(job, cpu)` runnable threads of it there.
    fn rates(&self, jobs: &[JobId], count: impl Fn(JobId, usize) -> u32) -> Vec<u128> {
        let weight = |j: JobId, k: usize| -> u128 {
            let d = count(j, k) as u128;
            if d == 0 {
                return 0;
            }
            let job = self.job(j);
            (job.weight as u128 * d / job.runnable as u128).max(1)
        };
        let load: [u128; 2] = [0, 1].map(|k| jobs.iter().map(|&j| weight(j, k)).sum());
        jobs.iter()
            .map(|&j| (0..2).filter(|&k| load[k] > 0).map(|k| (weight(j, k) << 32) / load[k]).sum())
            .collect()
    }

    /// The jobs on `change`'s two CPUs whose rate there it raises and those it
    /// lowers, each with its normalized service relative to the pair's first.
    fn winners_and_losers(&self, change: &Move) -> (Standing, Standing) {
        let (_, a, b) = change.threads[0].expect("a change moves a thread");
        let jobs = self.jobs_on(a, b);
        let cpus = [a, b];
        let count = |j: JobId, k: usize| -> u32 {
            let c = &self.cpus[cpus[k].0 as usize];
            c.members.iter().find(|g| g.job == j).map_or(0, |g| g.threads.len() as u32)
        };
        let before = self.rates(&jobs, count);
        let after = self.rates(&jobs, |j, k| {
            let mut n = count(j, k) as i64;
            for &(t, from, _) in change.threads.iter().flatten() {
                if self.thread(t).job == j {
                    n += if cpus[k] == from { -1 } else { 1 };
                }
            }
            n as u32
        });
        let reference = self.job(jobs[0]).served;
        let served = |i: usize| (jobs[i], lead(self.job(jobs[i]).served, reference));
        let gain = (0..jobs.len()).filter(|&i| after[i] > before[i]).map(served).collect();
        let loss = (0..jobs.len()).filter(|&i| after[i] < before[i]).map(served).collect();
        (gain, loss)
    }

    fn share_a_cpu(&self, x: JobId, y: JobId) -> bool {
        let (x, y) = (&self.job(x).cpus, &self.job(y).cpus);
        x.iter().any(|c| y.binary_search(c).is_ok())
    }

    /// Every allowed change between `a` and `b`, with the least lead a loser
    /// holds over the most-behind gainer.
    fn changes(&self, a: CpuId, b: CpuId) -> Vec<(i64, Move)> {
        let cpus = [a, b];
        let judge = |change: &Move| -> Option<i64> {
            let (gain, loss) = self.winners_and_losers(change);
            let &(behind_job, behind) = gain.iter().min_by_key(|g| g.1)?;
            let gap = loss.iter().map(|&(_, l)| l - behind).min()?;
            let apart = loss.iter().all(|&(job, _)| !self.share_a_cpu(job, behind_job));
            (gap > self.params.move_lead as i64 && apart).then_some(gap)
        };
        let on = [self.representatives(a), self.representatives(b)];
        let mut out = Vec::new();
        for k in 0..2 {
            for &t in &on[k] {
                let change = Move { threads: [Some((t, cpus[k], cpus[1 - k])), None] };
                if let Some(gap) = judge(&change) {
                    out.push((gap, change));
                }
            }
        }
        for &x in &on[0] {
            for &y in &on[1] {
                if self.thread(x).job == self.thread(y).job {
                    continue;
                }
                let change = Move { threads: [Some((x, a, b)), Some((y, b, a))] };
                if let Some(gap) = judge(&change) {
                    out.push((gap, change));
                }
            }
        }
        out
    }

    /// Make the change. The caller decides again for both CPUs.
    pub fn apply(&mut self, change: Move, now: Nanos) {
        for &(t, from, _) in change.threads.iter().flatten() {
            if self.current(from).is_some_and(|r| r.0 == t) {
                self.preempt(from, now);
            }
        }
        for &(t, _, to) in change.threads.iter().flatten() {
            self.migrate(t, to, now);
        }
    }

    pub fn served(&self, job: JobId) -> u64 {
        self.job(job).served
    }

    // ---- Inspection -----------------------------------------------------------

    /// The sum of `cpu`'s group weights: what balancing compares.
    pub fn load(&self, cpu: CpuId) -> u128 {
        self.cpus[cpu.0 as usize].groups.total_weight()
    }

    /// Runnable threads on `cpu` in every band, the running one included.
    pub fn queued(&self, cpu: CpuId) -> impl Iterator<Item = ThreadId> + '_ {
        let c = &self.cpus[cpu.0 as usize];
        c.rt.iter().copied().chain(c.members.iter().flat_map(|g| g.threads.keys())).chain(c.spare.iter().copied())
    }

    /// Whether no realtime or ordinary thread is runnable on `cpu`: what
    /// placement treats as idle, since spare work yields to anything else.
    pub fn idle_for_placement(&self, cpu: CpuId) -> bool {
        let c = &self.cpus[cpu.0 as usize];
        c.rt.is_empty() && c.members.is_empty()
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
            for (queue, band) in [(&cpu.rt, Band::Rt), (&cpu.spare, Band::Spare)] {
                for &t in queue {
                    assert_eq!(self.thread(t).state, State::Queued { cpu: here, band });
                    assert!(self.thread(t).granted, "{t:?} in the {band:?} band without a grant");
                    seen.push(t);
                }
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
                let holding: Vec<CpuId> =
                    (0..self.cpus.len() as u32).map(CpuId).filter(|&c| self.group_index(c, id).is_some()).collect();
                assert_eq!(job.cpus, holding, "{id:?}'s CPU set");
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
