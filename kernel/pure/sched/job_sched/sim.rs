//! The checkpoint simulator: [`JobSched`] on a machine of [`Topology`] CPUs,
//! driven by workloads, wake placement and a balancer, and measured against the
//! feasible weighted share.
//!
//! The reference is the fluid allocation: at each instant every job with `n`
//! runnable threads and weight `W` receives `min(n, λ·W)` CPUs, with `λ` filling
//! the machine (a thread uses at most one CPU; capacity a job cannot use goes
//! to the others). It is integrated exactly between events. Service is what the
//! simulated machine ran, attributed by who was on each CPU — not the model's
//! own counters.
//!
//! Placement stands in for stage 2's: a wake goes to the thread's last CPU if
//! idle, else the nearest idle CPU, else its last CPU; an idle CPU takes a
//! waiting thread from the CPU with the most; and every `balance_period` each
//! CPU takes from the most loaded CPU the waiting thread whose move most
//! reduces their difference in summed group weight, if any strictly does. With
//! `cross_cpu_lag`, every
//! `fairness_period` each CPU also makes the [`JobSched::fairness_move`] it is
//! offered against all others.

use super::*;
use crate::sched::topology::{CpuKeys, Topology};
use alloc::collections::{BTreeMap, BinaryHeap};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::cmp::Reverse;
use core::fmt::Write;


#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Work {
    Hog,
    /// Run `run` ns of CPU, sleep `sleep` ns, repeat.
    Duty { run: u64, sleep: u64 },
    /// Released every `period`; runs `work` and blocks.
    Periodic { period: u64, work: u64 },
}

/// Which partner CPUs each fairness step examines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Partners {
    Rotating,
    All,
}

struct Load {
    job: JobId,
    work: Work,
    /// CPU time left in the current burst.
    left: u64,
    home: CpuId,
    pinned: bool,
    /// Granted realtime by the scenario.
    rt: bool,
    finishing: bool,
    /// Periodic releases that found the previous burst unfinished.
    missed: u64,
    /// Latest completion after release, for periodic work.
    worst_response: u64,
    released: Nanos,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Ev {
    Wake(ThreadId),
    Release(ThreadId),
    Balance(CpuId),
    Fairness(CpuId),
    Churn,
}

pub struct Sim {
    pub s: JobSched,
    pub topo: Topology,
    pub now: Nanos,
    pub balance_period: u64,
    /// Changes made by [`JobSched::fairness_move`].
    pub fairness_moves: u64,
    /// Delay before a CPU running realtime work takes a decision: a
    /// non-preemptible section or late timer, for overrun accounting.
    pub rt_overrun: u64,
    churn: Option<(u64, u64)>,
    loads: BTreeMap<ThreadId, Load>,
    heap: BinaryHeap<Reverse<(Nanos, u64, Ev)>>,
    seq: u64,
    jobs: Vec<JobId>,
    weights: BTreeMap<JobId, u64>,
    /// Per measured job: CPU time received, split ordinary/realtime, and the
    /// fluid reference's ordinary share.
    pub got: BTreeMap<JobId, (u64, u64)>,
    pub ideal: BTreeMap<JobId, f64>,
    /// Per job: the service the per-CPU rule predicts — each CPU running
    /// ordinary work gives each job its group weight over the CPU's load.
    pub per_cpu_rule: BTreeMap<JobId, f64>,
    /// The longest a runnable ordinary thread waited queued without running,
    /// and the most runnable ordinary threads any CPU held.
    pub longest_wait: u64,
    pub most_threads: usize,
    /// The largest ratio of a thread's wait to its budget, `(slice +
    /// lag_bound) / share + 2·slice` — a request, plus the most debt it can
    /// bring, at its rate — where `share` is its rate on its CPU — its group weight over
    /// the CPU's load, divided among its siblings there — at its lowest
    /// during the wait.
    pub wait_over_budget: f64,
    /// Per thread: its current wait and the largest budget seen during it.
    waited: BTreeMap<ThreadId, (u64, f64)>,
    /// Ordinary service per CPU, for the realtime ceiling's protection.
    pub fair_on: Vec<u64>,
    pub rt_on: Vec<u64>,
    /// Spare-band time per CPU.
    pub spare_on: Vec<u64>,
    /// Per CPU, the longest stretch realtime ran without a break.
    pub rt_streak_max: Vec<u64>,
    rt_streak: Vec<u64>,
    rounds: Vec<u32>,
    pub partners: Partners,
    pub samples: Vec<(Nanos, Vec<(f64, f64)>)>,
    sample_every: u64,
    next_sample: Nanos,
    pub migrations: u64,
    pub switches: u64,
    steps: u64,
    /// Per CPU and `rt_interval` window aligned to the boundaries: service
    /// to threads the scenario never granted, whether such a thread was
    /// runnable throughout, and realtime execution.
    pub intervals: Vec<Vec<(u64, bool, u64)>>,
    window_ordinary: Vec<u64>,
    window_rt: Vec<u64>,
    window_waiting: Vec<bool>,
    window_end: Nanos,
}

pub fn flat(cpus: u32) -> Topology {
    Topology::new((0..cpus).map(|c| CpuKeys { core: c, llc: 0, package: 0 }).collect()).unwrap()
}

/// Four cores of two threads in one LLC, siblings four ids apart: the T14.
pub fn t14() -> Topology {
    Topology::new(
        [0u32, 2, 4, 6, 1, 3, 5, 7]
            .iter()
            .map(|&id| CpuKeys { core: id >> 1, llc: 0, package: 0 })
            .collect(),
    )
    .unwrap()
}

/// Two LLCs of four cores of two threads, siblings adjacent: the AMD bench.
pub fn amd() -> Topology {
    Topology::new((0..16u32).map(|id| CpuKeys { core: id >> 1, llc: id >> 3, package: 0 }).collect()).unwrap()
}

/// The fluid allocation: CPUs per job for `(weight, runnable)` pairs on `m`.
pub fn waterfill(jobs: &[(u64, u32)], m: f64) -> Vec<f64> {
    let mut rate = vec![0.0; jobs.len()];
    let mut order: Vec<usize> = (0..jobs.len()).filter(|&j| jobs[j].1 > 0).collect();
    order.sort_by(|&a, &b| {
        let ka = jobs[a].1 as f64 / jobs[a].0 as f64;
        let kb = jobs[b].1 as f64 / jobs[b].0 as f64;
        ka.partial_cmp(&kb).unwrap()
    });
    let mut cap = m;
    let mut wsum: f64 = order.iter().map(|&j| jobs[j].0 as f64).sum();
    for (k, &j) in order.iter().enumerate() {
        let fair = cap * jobs[j].0 as f64 / wsum;
        if (jobs[j].1 as f64) <= fair {
            rate[j] = jobs[j].1 as f64;
            cap -= rate[j];
            wsum -= jobs[j].0 as f64;
        } else {
            for &r in &order[k..] {
                rate[r] = cap * jobs[r].0 as f64 / wsum;
            }
            break;
        }
    }
    rate
}

impl Sim {
    pub fn new(topo: Topology, params: Params) -> Self {
        let m = topo.cpus();
        let mut sim = Self {
            s: JobSched::new(m, params, Nanos::ZERO),
            topo,
            now: Nanos::ZERO,
            balance_period: 4 * MS,
            fairness_moves: 0,
            rt_overrun: 0,
            churn: None,
            loads: BTreeMap::new(),
            heap: BinaryHeap::new(),
            seq: 0,
            jobs: Vec::new(),
            weights: BTreeMap::new(),
            got: BTreeMap::new(),
            ideal: BTreeMap::new(),
            per_cpu_rule: BTreeMap::new(),
            longest_wait: 0,
            most_threads: 0,
            wait_over_budget: 0.0,
            waited: BTreeMap::new(),
            fair_on: vec![0; m],
            rt_on: vec![0; m],
            spare_on: vec![0; m],
            rt_streak_max: vec![0; m],
            rt_streak: vec![0; m],
            rounds: vec![0; m],
            partners: Partners::Rotating,
            samples: Vec::new(),
            sample_every: MS,
            next_sample: Nanos::ZERO,
            migrations: 0,
            switches: 0,
            steps: 0,
            intervals: vec![Vec::new(); m],
            window_ordinary: vec![0; m],
            window_rt: vec![0; m],
            window_waiting: vec![true; m],
            window_end: Nanos(params.rt_interval),
        };
        for c in 0..m {
            let at = Nanos(sim.balance_period * c as u64 / m as u64 + 1);
            sim.push(at, Ev::Balance(CpuId(c as u32)));
            if params.cross_cpu_lag {
                let at = Nanos(params.fairness_period * c as u64 / m as u64 + 2);
                sim.push(at, Ev::Fairness(CpuId(c as u32)));
            }
        }
        sim
    }

    fn push(&mut self, at: Nanos, ev: Ev) {
        self.seq += 1;
        self.heap.push(Reverse((at, self.seq, ev)));
    }

    pub fn job(&mut self, weight: u64) -> JobId {
        let j = self.s.create_job(weight);
        self.jobs.push(j);
        self.weights.insert(j, weight);
        self.got.insert(j, (0, 0));
        self.ideal.insert(j, 0.0);
        self.per_cpu_rule.insert(j, 0.0);
        j
    }

    /// A thread of `job` doing `work`, made runnable now on `home` (or where
    /// placement puts it, unless `pinned`).
    pub fn thread(&mut self, job: JobId, work: Work, home: CpuId, pinned: bool) -> ThreadId {
        let t = self.s.spawn(job);
        let left = match work {
            Work::Hog => u64::MAX,
            Work::Duty { run, .. } => run,
            Work::Periodic { work, .. } => work,
        };
        self.loads.insert(
            t,
            Load { job, work, left, home, pinned, rt: false, finishing: false, missed: 0, worst_response: 0, released: self.now },
        );
        if let Work::Periodic { period, .. } = work {
            self.push(self.now.after(period), Ev::Release(t));
        }
        self.wake(t);
        t
    }

    pub fn grant(&mut self, t: ThreadId) {
        self.s.grant_rt(t, self.now).expect("a live worker of an unrevoked owner");
        self.loads.get_mut(&t).unwrap().rt = true;
        if let State::Queued { cpu, band: Band::Rt } = self.s.state(t) {
            if matches!(self.s.current(cpu), Some((_, Band::Fair))) {
                self.resched(cpu);
            }
        }
    }

    pub fn unpin(&mut self, t: ThreadId) {
        self.loads.get_mut(&t).unwrap().pinned = false;
    }

    /// Every `period`, move one queued, not running, thread of any band chosen
    /// by `seed` to another CPU chosen by it: migration beyond what balancing
    /// asks for.
    pub fn churn(&mut self, period: u64, seed: u64) {
        self.churn = Some((period, seed));
        self.push(self.now.after(period), Ev::Churn);
    }

    pub fn jobs(&self) -> &[JobId] {
        &self.jobs
    }

    pub fn weight(&self, job: JobId) -> u64 {
        self.weights[&job]
    }

    /// The thread exits the next time it runs.
    pub fn finish(&mut self, t: ThreadId) {
        self.loads.get_mut(&t).unwrap().finishing = true;
        if self.s.state(t) == State::Blocked {
            self.loads.remove(&t);
            self.s.exit(t);
        }
    }

    /// Over every run of `k` consecutive intervals in which ordinary work was
    /// runnable throughout: the least ordinary service any CPU received.
    pub fn ordinary_floor(&self, k: usize) -> u64 {
        let mut floor = u64::MAX;
        for cpu in &self.intervals {
            for w in cpu.windows(k) {
                if w.iter().all(|i| i.1) {
                    floor = floor.min(w.iter().map(|i| i.0).sum());
                }
            }
        }
        floor
    }

    /// Over every run of `k` consecutive intervals: the most realtime
    /// execution any CPU performed.
    pub fn rt_peak(&self, k: usize) -> u64 {
        self.intervals.iter().flat_map(|cpu| cpu.windows(k).map(|w| w.iter().map(|i| i.2).sum())).max().unwrap_or(0)
    }

    pub fn missed(&self, t: ThreadId) -> (u64, u64) {
        let l = &self.loads[&t];
        (l.missed, l.worst_response)
    }

    fn idle(&self, c: CpuId) -> bool {
        self.s.idle_for_placement(c)
    }

    /// The thread's home if it is idle, else the nearest idle CPU, else home.
    fn place(&self, t: ThreadId) -> CpuId {
        let l = &self.loads[&t];
        if l.pinned || self.idle(l.home) {
            return l.home;
        }
        (0..self.topo.cpus() as u32)
            .map(CpuId)
            .filter(|&c| self.idle(c))
            .min_by_key(|&c| (self.topo.level(l.home, c), c))
            .unwrap_or(l.home)
    }

    fn wake(&mut self, t: ThreadId) {
        let cpu = self.place(t);
        self.loads.get_mut(&t).unwrap().home = cpu;
        match self.s.wake(t, cpu, self.now) {
            Resched::Now => self.resched(cpu),
            Resched::At(_) | Resched::No => {}
        }
    }

    fn resched(&mut self, c: CpuId) {
        let before = self.s.current(c).map(|r| r.0);
        let mut next = self.s.schedule(c, self.now);
        // Spare work yields to ordinary work from anywhere.
        let spare = matches!(self.s.current(c), Some((_, Band::Spare)));
        if (next.is_none() || spare) && self.steal(c) {
            next = self.s.schedule(c, self.now);
        }
        if next != before {
            self.switches += 1;
        }
        if let Some(t) = next {
            self.loads.get_mut(&t).unwrap().home = c;
        }
    }

    /// Waiting (queued, not running) ordinary threads on `c` that may move.
    fn waiting(&self, c: CpuId) -> Vec<ThreadId> {
        let running = self.s.current(c).map(|r| r.0);
        self.s
            .queued(c)
            .filter(|&t| Some(t) != running)
            .filter(|&t| self.s.state(t) == (State::Queued { cpu: c, band: Band::Fair }))
            .filter(|t| !self.loads[t].pinned)
            .collect()
    }

    fn steal(&mut self, c: CpuId) -> bool {
        let src = (0..self.topo.cpus() as u32)
            .map(CpuId)
            .filter(|&s| s != c)
            .map(|s| (self.waiting(s).len(), s))
            .filter(|&(n, _)| n > 0)
            .max_by_key(|&(n, s)| (n, Reverse(self.topo.level(c, s)), Reverse(s)));
        let Some((_, s)) = src else { return false };
        let t = self.waiting(s)[0];
        self.s.migrate(t, c, self.now);
        self.migrations += 1;
        true
    }

    /// Under [`Partners::Rotating`], one partner per step, rotating through
    /// every other CPU: each CPU examines one pair per `fairness_period`, the
    /// machine `m`. [`Partners::All`] examines `m − 1` per CPU, `m(m−1)` in all.
    fn fairness_step(&mut self, c: CpuId) {
        let m = self.topo.cpus() as u32;
        if m < 2 {
            return;
        }
        let partners: Vec<CpuId> = match self.partners {
            Partners::All => (0..m).map(CpuId).collect(),
            Partners::Rotating => {
                let r = &mut self.rounds[c.0 as usize];
                *r = *r % (m - 1) + 1;
                vec![CpuId((c.0 + *r) % m)]
            }
        };
        let Some(change) = self.s.fairness_move(c, &partners) else { return };
        if change.threads.iter().flatten().any(|(t, _, _)| self.loads[t].pinned) {
            return;
        }
        self.s.apply(change, self.now);
        self.fairness_moves += 1;
        for &(_, from, to) in change.threads.iter().flatten() {
            self.migrations += 1;
            self.resched(from);
            self.resched(to);
        }
    }

    fn balance_step(&mut self, c: CpuId) {
        let src = (0..self.topo.cpus() as u32)
            .map(CpuId)
            .filter(|&s| s != c && !self.waiting(s).is_empty())
            .max_by_key(|&s| (self.s.load(s), Reverse(s)));
        let Some(src) = src else { return };
        let (ls, ld) = (self.s.load(src) as i128, self.s.load(c) as i128);
        let diff = ls - ld;
        let chosen = self
            .waiting(src)
            .into_iter()
            .map(|t| (t, self.s.thread_weight(self.s.job_of(t)) as i128))
            .filter(|&(_, w)| w < diff)
            .min_by_key(|&(t, w)| ((diff - 2 * w).abs(), t));
        if let Some((t, _)) = chosen {
            self.s.migrate(t, c, self.now);
            self.migrations += 1;
            if self.idle(c) {
                self.resched(c);
            }
        }
    }

    fn burst_end(&self, c: CpuId) -> Option<Nanos> {
        let (t, _) = self.s.current(c)?;
        let l = &self.loads[&t];
        if l.finishing {
            return Some(self.now);
        }
        (l.work != Work::Hog).then(|| self.now.after(l.left))
    }

    fn decision(&self, c: CpuId) -> Option<Nanos> {
        let d = self.s.next_decision(c).map(|d| match self.s.current(c) {
            Some((_, Band::Rt)) => d.after(self.rt_overrun),
            _ => d,
        });
        match (d, self.burst_end(c)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn advance(&mut self, t: Nanos) {
        let dt = t.since(self.now);
        if dt > 0 {
            let mut demand: BTreeMap<JobId, u32> = self.jobs.iter().map(|&j| (j, 0)).collect();
            for (&tid, l) in &self.loads {
                if matches!(self.s.state(tid), State::Queued { band: Band::Fair, .. }) {
                    *demand.entry(l.job).or_default() += 1;
                }
            }
            let rt_now = (0..self.topo.cpus() as u32)
                .filter(|&c| matches!(self.s.current(CpuId(c)), Some((_, Band::Rt))))
                .count();
            let pairs: Vec<(u64, u32)> = self.jobs.iter().map(|j| (self.weights[j], demand[j])).collect();
            let rates = waterfill(&pairs, (self.topo.cpus() - rt_now) as f64);
            for (j, r) in self.jobs.iter().zip(rates) {
                *self.ideal.get_mut(j).unwrap() += r * dt as f64;
            }
            // The per-CPU rule: on each CPU running ordinary work, a job gets
            // its group weight over the CPU's summed group weight.
            for c in 0..self.topo.cpus() {
                let cpu = CpuId(c as u32);
                if !matches!(self.s.current(cpu), Some((_, Band::Fair))) {
                    continue;
                }
                let load = self.s.load(cpu) as f64;
                for j in &self.jobs {
                    if let Some(w) = self.s.group_weight_on(cpu, *j) {
                        *self.per_cpu_rule.get_mut(j).unwrap() += w as f64 / load * dt as f64;
                    }
                }
            }
            // Per-thread waits: runnable ordinary threads not running.
            for c in 0..self.topo.cpus() {
                let cpu = CpuId(c as u32);
                let running = self.s.current(cpu).map(|r| r.0);
                let fair: Vec<ThreadId> = self
                    .s
                    .queued(cpu)
                    .filter(|&t| self.s.state(t) == (State::Queued { cpu, band: Band::Fair }))
                    .collect();
                self.most_threads = self.most_threads.max(fair.len());
                let load = self.s.load(cpu) as f64;
                let slice = self.s.params().slice as f64;
                let debt = self.s.params().lag_bound as f64;
                for &t in &fair {
                    let job = self.s.job_of(t);
                    let siblings = fair.iter().filter(|&&o| self.s.job_of(o) == job).count() as f64;
                    let share = self.s.group_weight_on(cpu, job).unwrap_or(1) as f64 / load / siblings;
                    let budget = (slice + debt) / share + 2.0 * slice;
                    let (w, most) = self.waited.entry(t).or_insert((0, 0.0));
                    if Some(t) == running {
                        (*w, *most) = (0, 0.0);
                    } else {
                        *w += dt;
                        *most = f64::max(*most, budget);
                        self.longest_wait = self.longest_wait.max(*w);
                        self.wait_over_budget = self.wait_over_budget.max(*w as f64 / *most);
                    }
                }
            }
            for c in 0..self.topo.cpus() {
                let cpu = CpuId(c as u32);
                let ordinary_runnable = self.s.queued(cpu).any(|t| !self.loads[&t].rt);
                let band_now = self.s.current(cpu).map(|r| r.1);
                if band_now == Some(Band::Rt) {
                    self.rt_streak[c] += dt;
                    self.rt_streak_max[c] = self.rt_streak_max[c].max(self.rt_streak[c]);
                } else {
                    self.rt_streak[c] = 0;
                }
                if let Some((tid, band)) = self.s.current(cpu) {
                    let l = self.loads.get_mut(&tid).unwrap();
                    if l.work != Work::Hog {
                        l.left = l.left.saturating_sub(dt);
                    }
                    let g = self.got.get_mut(&l.job).unwrap();
                    match band {
                        Band::Fair => {
                            g.0 += dt;
                            self.fair_on[c] += dt;
                            if !l.rt {
                                self.window_ordinary[c] += dt;
                            }
                        }
                        Band::Rt => {
                            g.1 += dt;
                            self.rt_on[c] += dt;
                            self.window_rt[c] += dt;
                        }
                        Band::Spare => self.spare_on[c] += dt,
                    }
                }
                self.window_waiting[c] &= ordinary_runnable;
            }
        }
        self.now = t;
        if self.now == self.window_end {
            for c in 0..self.topo.cpus() {
                self.intervals[c].push((self.window_ordinary[c], self.window_waiting[c], self.window_rt[c]));
                self.window_ordinary[c] = 0;
                self.window_rt[c] = 0;
                self.window_waiting[c] = true;
            }
            self.window_end = self.window_end.after(self.s.params().rt_interval);
        }
        if self.now == self.next_sample {
            let row = self.jobs.iter().map(|j| ((self.got[j].0 + self.got[j].1) as f64, self.ideal[j])).collect();
            self.samples.push((self.now, row));
            self.next_sample = self.next_sample.after(self.sample_every);
        }
    }

    /// Run until `end`, checking the model's invariants as it goes.
    pub fn run_until(&mut self, end: Nanos) {
        while self.now < end {
            let mut t = end.min(self.next_sample).min(self.window_end);
            for c in 0..self.topo.cpus() as u32 {
                if let Some(d) = self.decision(CpuId(c)) {
                    t = t.min(d.max(self.now));
                }
            }
            if let Some(Reverse((et, _, _))) = self.heap.peek() {
                t = t.min(*et);
            }
            self.advance(t);
            for c in 0..self.topo.cpus() as u32 {
                self.cpu_due(CpuId(c));
            }
            while let Some(&Reverse((et, _, ev))) = self.heap.peek() {
                if et > self.now {
                    break;
                }
                self.heap.pop();
                self.handle(ev);
            }
            self.steps += 1;
            if self.steps.is_multiple_of(64) {
                self.s.check();
            }
        }
        self.s.check();
    }

    fn cpu_due(&mut self, c: CpuId) {
        if let Some((t, _)) = self.s.current(c) {
            let l = &self.loads[&t];
            let done = l.finishing || (l.work != Work::Hog && l.left == 0);
            if done {
                let t = self.s.block(c, self.now);
                let l = self.loads.get_mut(&t).unwrap();
                if l.finishing {
                    self.loads.remove(&t);
                    self.s.exit(t);
                } else {
                    let response = self.now.since(l.released);
                    l.worst_response = l.worst_response.max(response);
                    if let Work::Duty { run, sleep } = l.work {
                        l.left = run;
                        let at = self.now.after(sleep);
                        self.push(at, Ev::Wake(t));
                    }
                }
                self.resched(c);
                return;
            }
        }
        if self.decision(c).is_some_and(|d| d <= self.now) {
            self.resched(c);
            assert!(
                self.decision(c).is_none_or(|d| d > self.now),
                "{c:?} keeps asking for a decision at {:?}",
                self.now
            );
        }
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Wake(t) => {
                if self.loads.contains_key(&t) {
                    self.wake(t);
                }
            }
            Ev::Release(t) => {
                let Some(l) = self.loads.get_mut(&t) else { return };
                let Work::Periodic { period, work } = l.work else { unreachable!() };
                if l.finishing {
                    return;
                }
                let next = self.now.after(period);
                if self.s.state(t) == State::Blocked {
                    l.left = work;
                    l.released = self.now;
                    self.push(next, Ev::Release(t));
                    self.wake(t);
                } else {
                    l.missed += 1;
                    self.push(next, Ev::Release(t));
                }
            }
            Ev::Churn => {
                let (period, seed) = self.churn.unwrap();
                let seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                self.churn = Some((period, seed));
                let m = self.topo.cpus() as u64;
                let from = CpuId(((seed >> 33) % m) as u32);
                let to = CpuId(((seed >> 13) % m) as u32);
                let running = self.s.current(from).map(|r| r.0);
                let waiting: Vec<ThreadId> =
                    self.s.queued(from).filter(|&t| Some(t) != running && !self.loads[&t].pinned).collect();
                if from != to && !waiting.is_empty() {
                    let t = waiting[((seed >> 40) as usize) % waiting.len()];
                    self.s.migrate(t, to, self.now);
                    self.migrations += 1;
                    if self.idle(to) {
                        self.resched(to);
                    }
                }
                let at = self.now.after(period);
                self.push(at, Ev::Churn);
            }
            Ev::Fairness(c) => {
                self.fairness_step(c);
                let at = self.now.after(self.s.params().fairness_period);
                self.push(at, Ev::Fairness(c));
            }
            Ev::Balance(c) => {
                self.balance_step(c);
                let at = self.now.after(self.balance_period);
                self.push(at, Ev::Balance(c));
            }
        }
    }

    /// The largest difference, in ms, between a job's service and its fluid
    /// share over any window of `window` ns starting at or after `from`.
    pub fn window_error(&self, job: usize, window: u64, from: Nanos) -> f64 {
        let k = (window / self.sample_every) as usize;
        let start = self.samples.iter().position(|(t, _)| *t >= from).unwrap_or(self.samples.len());
        let mut worst: f64 = 0.0;
        for i in start..self.samples.len().saturating_sub(k) {
            let (a0, i0) = self.samples[i].1[job];
            let (a1, i1) = self.samples[i + k].1[job];
            worst = worst.max(((a1 - a0) - (i1 - i0)).abs());
        }
        worst / MS as f64
    }

    /// One line per job: share received, its fluid share, and the worst window
    /// error at each window length.
    pub fn report(&self, title: &str, from: Nanos, windows: &[u64]) -> String {
        let mut out = String::new();
        let span = self.now.since(from) as f64;
        let _ = writeln!(out, "{title}: t={}ms migrations={} switches={}", self.now.0 / MS, self.migrations, self.switches);
        let mut header = String::from("  job  weight   CPUs(got)  CPUs(fluid)");
        for w in windows {
            let _ = write!(header, "  err@{}ms", w / MS);
        }
        let _ = writeln!(out, "{header}");
        let base = self.samples.iter().find(|(t, _)| *t >= from).map(|s| s.1.clone());
        let last = &self.samples.last().unwrap().1;
        for (j, job) in self.jobs.iter().enumerate() {
            let (a0, i0) = base.as_ref().map_or((0.0, 0.0), |b| b[j]);
            let got = (last[j].0 - a0) / span;
            let fluid = (last[j].1 - i0) / span;
            let mut line = format!("  {j:>3}  {:>6.2}  {got:>10.3}  {fluid:>11.3}", self.weights[job] as f64 / WEIGHT_UNIT as f64);
            for &w in windows {
                let _ = write!(line, "  {:>8.2}", self.window_error(j, w, from));
            }
            let _ = writeln!(out, "{line}");
        }
        out
    }
}
