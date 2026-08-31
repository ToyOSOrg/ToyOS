//! The host's process table: everything the kernel's is, minus everything a
//! lifecycle decision cannot name.
//!
//! `#[cfg(test)]`, so none of it reaches a kernel build. What it adds beyond
//! the two traits is the *consequences* a decision hands back and the kernel
//! performs — a completion post, a `retire_task`, a `publish_exit`, an idle
//! pass taking an entry — because the laws worth checking are about the order
//! those happen in, and a model that only held the two states could not see
//! one.
//!
//! A `BTreeMap` where the kernel has a `hashbrown::HashMap`: nothing here
//! depends on the order, and a model whose counter-example is different every
//! run is a model nobody can bisect.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::table::{Lifecycle, Processes};
use crate::{Pid, ThreadLocation, Tid, Watch};

/// One process, as its lifecycle sees it.
#[derive(Clone)]
pub struct ModelProc {
    main_tid: Tid,
    tearing_down: bool,
    threads: BTreeMap<Tid, ThreadLocation>,
    next_tid: Tid,
    /// How many times a claim was raised on this process. Counted here because
    /// `begin_teardown` has exactly one caller and the count is the law.
    claims: u32,
}

impl Lifecycle for ModelProc {
    fn main_tid(&self) -> Tid {
        self.main_tid
    }
    fn tearing_down(&self) -> bool {
        self.tearing_down
    }
    fn begin_teardown(&mut self) {
        self.tearing_down = true;
        self.claims += 1;
    }
    fn location(&self, tid: Tid) -> Option<ThreadLocation> {
        self.threads.get(&tid).copied()
    }
    fn set_location(&mut self, tid: Tid, to: ThreadLocation) {
        if let Some(slot) = self.threads.get_mut(&tid) {
            *slot = to;
        }
    }
    fn forget_thread(&mut self, tid: Tid) {
        self.threads.remove(&tid);
    }
    fn each_thread(&self, f: &mut dyn FnMut(Tid, ThreadLocation)) {
        for (&tid, &at) in &self.threads {
            f(tid, at);
        }
    }
}

/// The table, plus the effects the kernel would have performed.
#[derive(Clone)]
pub struct World {
    procs: BTreeMap<Pid, ModelProc>,
    next_pid: Pid,
    /// The exit each process published, which is what `published_exit` reads —
    /// on the object in the kernel, and never on the entry.
    published: BTreeMap<Pid, i32>,
    /// Waiters: the subject armed on, and the thread that armed.
    waiters: BTreeSet<(Watch, Pid, Tid)>,
    /// Waiters a post has released.
    released: BTreeSet<(Watch, Pid, Tid)>,
    /// Threads `scheduler::retire_task` has taken off every CPU. A thread not
    /// in here may still be picked and run.
    retired: BTreeSet<(Pid, Tid)>,
    /// Threads past the point of no return but not yet dropped by an exit
    /// pass: a claimant inside its own teardown, which cannot retire itself and
    /// will not reach Ring 3 again either.
    leaving: BTreeSet<(Pid, Tid)>,
    /// Entries an idle pass has taken out of the table.
    reaped: BTreeSet<Pid>,
    /// TLS blocks a spawn's phase 2 mapped and no thread owns yet.
    tls_mapped: BTreeSet<u32>,
    next_tls: u32,
}

impl Processes for World {
    type Proc = ModelProc;

    fn get(&self, pid: Pid) -> Option<&ModelProc> {
        self.procs.get(&pid)
    }
    fn get_mut(&mut self, pid: Pid) -> Option<&mut ModelProc> {
        self.procs.get_mut(&pid)
    }
    fn published_exit(&self, pid: Pid) -> bool {
        self.published.contains_key(&pid)
    }
    fn each_pid(&self, f: &mut dyn FnMut(Pid)) {
        for &pid in self.procs.keys() {
            f(pid);
        }
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            procs: BTreeMap::new(),
            next_pid: Pid(1),
            published: BTreeMap::new(),
            waiters: BTreeSet::new(),
            released: BTreeSet::new(),
            retired: BTreeSet::new(),
            leaving: BTreeSet::new(),
            reaped: BTreeSet::new(),
            tls_mapped: BTreeSet::new(),
            next_tls: 0,
        }
    }

    /// `spawn_thread`'s phase 2: a TLS block mapped, owned by no thread until the insert adopts it.
    pub fn map_tls(&mut self) -> u32 {
        let block = self.next_tls;
        self.next_tls += 1;
        self.tls_mapped.insert(block);
        block
    }

    /// The insert handing the block to the new thread, whose teardown owns it.
    pub fn adopt_tls(&mut self, block: u32) {
        self.tls_mapped.remove(&block);
    }

    /// `MappedPages::release` on a refused spawn: unmapped before the pages go back.
    pub fn release_tls(&mut self, block: u32) {
        self.tls_mapped.remove(&block);
    }

    /// A process with one thread, which is its main one — what
    /// `ProcessEntry::new` builds.
    pub fn spawn_process(&mut self) -> Pid {
        let pid = self.next_pid;
        self.next_pid = Pid(pid.0 + 1);
        let mut threads = BTreeMap::new();
        threads.insert(Tid(0), ThreadLocation::Scheduled);
        self.procs.insert(
            pid,
            ModelProc {
                main_tid: Tid(0),
                tearing_down: false,
                threads,
                next_tid: Tid(1),
                claims: 0,
            },
        );
        pid
    }

    /// Insert a thread the way `spawn_thread`'s phase 3 does — in the table and
    /// enqueued in the scheduler, so it is alive and unretired.
    pub fn spawn_thread(&mut self, pid: Pid) -> Tid {
        let proc = self.procs.get_mut(&pid).expect("spawn_thread on a live process");
        let tid = proc.next_tid;
        proc.next_tid = Tid(tid.0 + 1);
        proc.threads.insert(tid, ThreadLocation::Scheduled);
        tid
    }

    pub fn main_tid(&self, pid: Pid) -> Tid {
        self.procs[&pid].main_tid
    }

    pub fn set_location(&mut self, pid: Pid, tid: Tid, to: ThreadLocation) {
        if let Some(proc) = self.procs.get_mut(&pid) {
            proc.set_location(tid, to);
        }
    }

    pub fn forget_thread(&mut self, pid: Pid, tid: Tid) {
        if let Some(proc) = self.procs.get_mut(&pid) {
            proc.forget_thread(tid);
        }
    }

    /// `completion::wait_until` — a waiter registered on a subject.
    pub fn arm(&mut self, on: Watch, waiter: (Pid, Tid)) {
        self.waiters.insert((on, waiter.0, waiter.1));
    }

    /// `completion::post` — every waiter on this subject runs again.
    pub fn post(&mut self, on: Watch) {
        let hit: Vec<(Watch, Pid, Tid)> =
            self.waiters.iter().filter(|(w, _, _)| *w == on).copied().collect();
        for entry in hit {
            self.released.insert(entry);
        }
    }

    pub fn released(&self, on: Watch, waiter: (Pid, Tid)) -> bool {
        self.released.contains(&(on, waiter.0, waiter.1))
    }

    /// Waiters nothing has released.
    pub fn stranded(&self) -> Vec<(Watch, Pid, Tid)> {
        self.waiters.difference(&self.released).copied().collect()
    }

    /// `scheduler::retire_task` — the thread is provably off every CPU, its
    /// payload dropped, and `publish_released` has posted `Gone` on its own
    /// watch.
    ///
    /// **The post is not an extra the model added.** `KernelPayload`'s release
    /// sink runs exactly once per task and ends with
    /// `TaskHandle::publish_released`, whose post is on the thread's own watch
    /// — "the same subject a joiner uses, and the reason the release no longer
    /// needs a queue of its own" (`kernel/src/sched/payload.rs`). A model
    /// without it strands joiners the kernel releases.
    pub fn retire(&mut self, pid: Pid, tid: Tid) {
        self.retired.insert((pid, tid));
        self.post(Watch::Thread(pid, tid));
    }

    /// A thread that has committed to leaving but whose payload the exit pass
    /// has not dropped yet — a teardown claimant, running the teardown on its
    /// own kernel stack. It will not reach Ring 3 again, so it is as unable to
    /// touch a freed mapping as a retired thread; what it has not done yet is
    /// release anybody.
    pub fn leaving(&mut self, pid: Pid, tid: Tid) {
        self.leaving.insert((pid, tid));
    }

    pub fn is_retired(&self, pid: Pid, tid: Tid) -> bool {
        self.retired.contains(&(pid, tid))
    }

    /// Whether this thread can still execute user code.
    fn runnable(&self, pid: Pid, tid: Tid) -> bool {
        !self.retired.contains(&(pid, tid))
            && !self.leaving.contains(&(pid, tid))
            && self
                .procs
                .get(&pid)
                .and_then(|p| p.location(tid))
                .is_some_and(|at| !at.is_zombie())
    }

    /// `ProcessObject::publish_exit`, assertion and all: two publishes mean two
    /// teardowns claimed one process.
    pub fn publish_exit(&mut self, pid: Pid, code: i32) {
        assert!(
            self.published.insert(pid, code).is_none(),
            "pid {pid} published two exits",
        );
        self.post(Watch::Process(pid));
    }

    /// The idle pass taking an entry, which is what `reap_finished` returns for
    /// the caller to drop.
    pub fn reap(&mut self, pid: Pid) {
        self.procs.remove(&pid);
        self.reaped.insert(pid);
    }

    pub fn was_reaped(&self, pid: Pid) -> bool {
        self.reaped.contains(&pid)
    }

    /// **The laws, checked at every state a step leaves behind.**
    ///
    /// Each is a sentence the kernel already states somewhere and nothing but a
    /// booted guest could check.
    pub fn faults(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (&pid, proc) in &self.procs {
            // L1. One process, one teardown, one exit. `publish_exit` asserts
            // the other half of this; this is the half that can be seen before
            // the publish happens.
            if proc.claims > 1 {
                out.push(alloc::format!("pid {pid}: {} teardown claims succeeded", proc.claims));
            }
            if self.published.contains_key(&pid) {
                for (&tid, &at) in &proc.threads {
                    // L2. An exit is published only once every thread of the
                    // process is dead.
                    if !at.is_zombie() {
                        out.push(alloc::format!(
                            "pid {pid} published its exit with tid {tid} still Scheduled",
                        ));
                    }
                    // L3. **And only once every thread is provably off every
                    // CPU.** The stronger half, and the one a thread inserted
                    // behind a retire sweep breaks: the marks are a table write
                    // and reach a thread the sweep never named, the retire is
                    // what stops it running.
                    if !self.retired.contains(&(pid, tid)) && !self.leaving.contains(&(pid, tid)) {
                        out.push(alloc::format!(
                            "pid {pid} published its exit with tid {tid} never retired",
                        ));
                    }
                }
            }
        }
        out
    }

    /// [`Self::faults`], plus **L4** — a waiter is released by the subject it
    /// named — which can only be judged once every scripted operation has run
    /// to its end. A join that has not been answered *yet* is the ordinary
    /// case, so checking it at every state would report every schedule.
    /// And **L5** — every TLS block a spawn mapped ends owned or released.
    pub fn final_faults(&self) -> Vec<String> {
        let mut out = self.faults();
        for &block in &self.tls_mapped {
            out.push(alloc::format!(
                "TLS block {block} is mapped and owned by nobody — a refused spawn dropped it \
                 without unmapping",
            ));
        }
        for (watch, waiter_pid, waiter_tid) in self.stranded() {
            // A waiter the machine has already given up on is not stranded: its
            // own process is being torn down and it is going with it. The
            // strand that matters is a thread that *will* run again and has
            // nothing left to wake it.
            if !self.runnable(waiter_pid, waiter_tid) {
                continue;
            }
            // A waiter on a subject that is *gone* is a thread that never runs
            // again. A waiter on one still alive is just waiting.
            let dead = match watch {
                Watch::Thread(pid, tid) => self
                    .procs
                    .get(&pid)
                    .is_none_or(|p| p.location(tid).is_none_or(ThreadLocation::is_zombie)),
                Watch::Process(pid) => self.published.contains_key(&pid),
            };
            if dead {
                out.push(alloc::format!(
                    "pid {waiter_pid} tid {waiter_tid} armed on {watch:?}, which has ended, \
                     and nothing released it",
                ));
            }
        }
        out
    }
}
