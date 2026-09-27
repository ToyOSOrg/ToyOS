//! The host's process table: everything the kernel's is, minus everything a
//! lifecycle decision cannot name.
//!
//! `#[cfg(test)]`, so none of it reaches a kernel build. What it adds beyond
//! the two traits is the *consequences* a decision hands back and the kernel
//! performs — a watch's post, a retire, a `publish_exit`, an idle
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
use crate::teardown::{self, Leave};
use crate::{Pid, ThreadLocation, Tid, Watch};

/// One process, as its lifecycle sees it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ModelProc {
    main_tid: Tid,
    teardown_code: Option<i32>,
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
    fn teardown_code(&self) -> Option<i32> {
        self.teardown_code
    }
    fn begin_teardown(&mut self, code: i32) {
        self.teardown_code = Some(code);
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

/// Where a thread is on its way out, one lock section or pass per step.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Out {
    /// `process::leave`'s table section.
    Leave,
    /// The last one out: `teardown_resources`, the process's mappings and handles.
    Free { code: i32 },
    /// The last one out: `ProcessObject::publish_exit`.
    Publish { code: i32 },
    /// `thread_exit`'s post on its own watch.
    Post,
    /// The exit pass switches away from the thread.
    Switch,
    /// The next pass on that CPU frees the payload, kernel stack and all.
    Release,
    Gone,
}

/// A thread leaving: the code it chose, and how far it is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Departure {
    chosen: Option<i32>,
    at: Out,
}

/// The table, plus the effects the kernel would have performed.
#[derive(Clone, PartialEq, Eq, Hash)]
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
    /// Threads a retire was posted on: the kill bit.
    killed: BTreeSet<(Pid, Tid)>,
    /// Threads inside a scripted operation, which reach no safe point until it returns.
    in_kernel: BTreeSet<(Pid, Tid)>,
    /// Threads on their way out, and how far each is.
    departing: BTreeMap<(Pid, Tid), Departure>,
    /// Threads the scheduler has switched away from for the last time.
    switched: BTreeSet<(Pid, Tid)>,
    /// Threads whose kernel stack has been freed.
    stacks_freed: BTreeSet<(Pid, Tid)>,
    /// Threads that died in panic recovery and never run again.
    poisoned: BTreeSet<(Pid, Tid)>,
    /// Threads whose own TLS block is still mapped in their process.
    mapped: BTreeSet<(Pid, Tid)>,
    /// Threads whose entry went, and its mapped TLS block with it, while a
    /// sibling was still in the process.
    unmapped_under_siblings: BTreeSet<(Pid, Tid)>,
    /// How many times each process's resources were freed.
    frees: BTreeMap<Pid, u32>,
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
            killed: BTreeSet::new(),
            in_kernel: BTreeSet::new(),
            departing: BTreeMap::new(),
            switched: BTreeSet::new(),
            stacks_freed: BTreeSet::new(),
            poisoned: BTreeSet::new(),
            mapped: BTreeSet::new(),
            unmapped_under_siblings: BTreeSet::new(),
            frees: BTreeMap::new(),
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
                teardown_code: None,
                threads,
                next_tid: Tid(1),
                claims: 0,
            },
        );
        self.mapped.insert((pid, Tid(0)));
        pid
    }

    /// Insert a thread the way `spawn_thread`'s phase 3 does — in the table and
    /// enqueued in the scheduler, so it is alive and in the process.
    pub fn spawn_thread(&mut self, pid: Pid) -> Tid {
        let proc = self.procs.get_mut(&pid).expect("spawn_thread on a live process");
        let tid = proc.next_tid;
        proc.next_tid = Tid(tid.0 + 1);
        proc.threads.insert(tid, ThreadLocation::Scheduled);
        self.mapped.insert((pid, tid));
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

    /// The `ThreadEntry` a join collected is dropped, and whatever its
    /// `ThreadData` still maps goes back with it.
    pub fn drop_collected(&mut self, pid: Pid, tid: Tid) {
        let mut still_in = false;
        if let Some(proc) = self.procs.get(&pid) {
            proc.each_thread(&mut |_, at| still_in |= !at.is_zombie());
        }
        if self.mapped.remove(&(pid, tid)) && still_in {
            self.unmapped_under_siblings.insert((pid, tid));
        }
    }

    /// `release_thread`: a thread's own exit unmaps its TLS block before it leaves.
    pub fn unmap_own(&mut self, pid: Pid, tid: Tid) {
        self.mapped.remove(&(pid, tid));
    }

    /// `watch::wait_until` — a waiter registered on a subject's watch.
    pub fn arm(&mut self, on: Watch, waiter: (Pid, Tid)) {
        self.waiters.insert((on, waiter.0, waiter.1));
    }

    /// `Watch::post` — every waiter registered on this subject runs again.
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

    /// `scheduler::post_retire`: the kill bit. A thread in Ring 3 leaves at its
    /// next exit boundary, one in a scripted operation when the operation returns.
    pub fn post_retire(&mut self, pid: Pid, tid: Tid) {
        assert!(self.killed.insert((pid, tid)), "a second retirer for pid {pid} tid {tid}");
    }

    /// A thread starting a scripted operation.
    pub fn enter_kernel(&mut self, by: (Pid, Tid)) {
        self.in_kernel.insert(by);
    }

    /// Its operation returned.
    pub fn leave_kernel(&mut self, by: (Pid, Tid)) {
        self.in_kernel.remove(&by);
    }

    /// A thread in a scripted operation starting its own way out.
    pub fn depart(&mut self, pid: Pid, tid: Tid, chosen: Option<i32>) {
        assert!(
            self.departing.insert((pid, tid), Departure { chosen, at: Out::Leave }).is_none(),
            "pid {pid} tid {tid} started out twice",
        );
        self.in_kernel.remove(&(pid, tid));
    }

    /// Every thread with a step of its way out it can take now: one already on
    /// its way, and a killed one at its exit boundary.
    pub fn ready_departures(&self) -> Vec<(Pid, Tid)> {
        let mut ready: Vec<(Pid, Tid)> = self
            .departing
            .iter()
            .filter(|(_, d)| d.at != Out::Gone)
            .map(|(&id, _)| id)
            .collect();
        for &(pid, tid) in &self.killed {
            let at_boundary = !self.in_kernel.contains(&(pid, tid))
                && !self.departing.contains_key(&(pid, tid))
                && !self.poisoned.contains(&(pid, tid))
                && self.procs.get(&pid).and_then(|p| p.location(tid)) == Some(ThreadLocation::Scheduled);
            if at_boundary {
                ready.push((pid, tid));
            }
        }
        ready
    }

    /// One step of a thread's way out.
    pub fn depart_step(&mut self, pid: Pid, tid: Tid) {
        let departure = self.departing.entry((pid, tid)).or_insert(Departure { chosen: None, at: Out::Leave });
        let chosen = departure.chosen;
        let next = match departure.at {
            Out::Leave => match teardown::leave(self, pid, tid, chosen) {
                Leave::Last { code } => Out::Free { code },
                Leave::NotLast => Out::Post,
            },
            Out::Free { code } => {
                *self.frees.entry(pid).or_insert(0) += 1;
                self.mapped.retain(|&(p, _)| p != pid);
                Out::Publish { code }
            }
            Out::Publish { code } => {
                self.publish_exit(pid, code);
                Out::Post
            }
            Out::Post => {
                if chosen.is_some() {
                    self.post(Watch::Thread(pid, tid));
                }
                Out::Switch
            }
            Out::Switch => {
                self.switched.insert((pid, tid));
                Out::Release
            }
            Out::Release => {
                self.free_stack(pid, tid);
                Out::Gone
            }
            Out::Gone => unreachable!("a thread that is gone has no step left"),
        };
        self.departing.get_mut(&(pid, tid)).expect("inserted above").at = next;
    }

    /// `Hw::release`: the payload goes, and the kernel stack with it.
    pub fn free_stack(&mut self, pid: Pid, tid: Tid) {
        self.stacks_freed.insert((pid, tid));
    }

    /// A thread dying in panic recovery: `schedule_no_return`'s exit pass, and
    /// the release after it.
    pub fn poison(&mut self, pid: Pid, tid: Tid) {
        self.in_kernel.remove(&(pid, tid));
        self.poisoned.insert((pid, tid));
        self.switched.insert((pid, tid));
        self.free_stack(pid, tid);
    }

    /// Whether this thread can still execute user code.
    fn runnable(&self, pid: Pid, tid: Tid) -> bool {
        !self.stacks_freed.contains(&(pid, tid))
            && !self.departing.contains_key(&(pid, tid))
            && !self.killed.contains(&(pid, tid))
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

    pub fn published(&self, pid: Pid) -> Option<i32> {
        self.published.get(&pid).copied()
    }

    /// The idle pass taking an entry, which is what `reap_finished` returns for
    /// the caller to drop.
    pub fn reap(&mut self, pid: Pid) {
        self.procs.remove(&pid);
    }

    /// **The laws, checked at every state a step leaves behind.**
    ///
    /// Each is a sentence the kernel already states somewhere and nothing but a
    /// booted guest could check.
    pub fn faults(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (&pid, proc) in &self.procs {
            // L1. One process, one claim, one teardown. `publish_exit` asserts
            // the publish half.
            if proc.claims > 1 {
                out.push(alloc::format!("pid {pid}: {} teardown claims succeeded", proc.claims));
            }
            let frees = self.frees.get(&pid).copied().unwrap_or(0);
            if frees > 1 {
                out.push(alloc::format!("pid {pid}: its resources were freed {frees} times"));
            }
            // L2. Nothing a thread can still run in is freed or published
            // before every thread has left.
            if frees > 0 || self.published.contains_key(&pid) {
                for (&tid, &at) in &proc.threads {
                    if !at.is_zombie() {
                        out.push(alloc::format!(
                            "pid {pid} was torn down with tid {tid} still in it",
                        ));
                    }
                }
            }
        }
        // L3. A thread's stack is freed only once the scheduler has switched
        // away from it.
        for id in self.stacks_freed.difference(&self.switched) {
            out.push(alloc::format!("pid {} tid {}: its stack was freed while it ran on it", id.0, id.1));
        }
        // L8. No thread's mappings go while a sibling can still run in them.
        for (pid, tid) in &self.unmapped_under_siblings {
            out.push(alloc::format!(
                "pid {pid} tid {tid}: its entry and its mapped TLS went while a sibling was still in the process",
            ));
        }
        out
    }

    /// [`Self::faults`], plus **L4** — a waiter is released by the subject it
    /// named — which can only be judged once every scripted operation has run
    /// to its end. A join that has not been answered *yet* is the ordinary
    /// case, so checking it at every state would report every schedule.
    /// And **L5** — every TLS block a spawn mapped ends owned or released.
    /// And **L6** — every claimed process is torn down: its exit published,
    /// every killed thread gone.
    pub fn final_faults(&self) -> Vec<String> {
        let mut out = self.faults();
        for (&pid, proc) in &self.procs {
            if proc.claims > 0 && !self.published.contains_key(&pid) {
                out.push(alloc::format!("pid {pid} was claimed for teardown and never published an exit"));
            }
        }
        for &(pid, tid) in &self.killed {
            if !self.stacks_freed.contains(&(pid, tid)) && self.procs.get(&pid).is_some_and(|p| p.location(tid).is_some()) {
                out.push(alloc::format!("pid {pid} tid {tid} was killed and never left"));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The teeth behind L3: a stack freed before the switch is what it reports.
    /// Run by hand, since no kernel path the model scripts frees one early.
    #[test]
    fn a_stack_freed_before_the_switch_is_what_l3_reports() {
        let mut world = World::new();
        let pid = world.spawn_process();
        world.free_stack(pid, world.main_tid(pid));
        let faults = world.faults();
        assert!(
            faults.iter().any(|f| f.contains("freed while it ran on it")),
            "L3 cannot see a stack freed under its thread: {faults:?}",
        );
    }
}
