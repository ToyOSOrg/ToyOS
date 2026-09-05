//! Every ordering of two CPUs inside one process's lifecycle.
//!
//! **This is the file the crate exists for.** `kernel/src/process.rs` gives the
//! process table lock up between every phase of a teardown and between both
//! halves of a spawn, and its comments say what each window costs — "a thread
//! enqueued now would be invisible to its retire sweep", "the current thread is
//! running this and cannot retire itself", "once it is published the entry is
//! reapable, so nothing may read the table for this pid after this point". Not
//! one of those sentences was checkable by anything but a booted guest with a
//! race that had to land the wrong way, which is why
//! `issues/kernel/spawned-process-never-starts.md` has been open since August
//! and was never reproduced in QEMU.
//!
//! An [`Op`] here is one of those kernel paths, cut at exactly the points where
//! the real one drops the lock, carrying the same values across the gap that
//! the real one carries in locals. [`explore`] runs every interleaving of a set
//! of them and checks `World::faults` at every state — depth-first, exhaustive,
//! and in milliseconds.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::model::World;
use crate::table::{Lifecycle, Processes};
use crate::{join, reap, spawn, teardown, Pid, ThreadLocation, Tid, Watch};

/// One kernel path, mid-flight.
///
/// Each variant's `pc` is the number of lock sections it has completed, and
/// every field beside it is a value the real path carries in a local across a
/// lock release — which is the whole reason a window exists to explore.
#[derive(Clone, Debug)]
pub enum Op {
    /// `process::release_process` + `teardown_tail`: a thread ending its own
    /// process.
    Exit { pid: Pid, tid: Tid, code: i32, pc: u32, others: Vec<Tid>, next: usize },
    /// `process::kill_process`: a handle holder ending somebody else's.
    Kill { pid: Pid, code: i32, pc: u32, tids: Vec<Tid>, next: usize },
    /// `process::spawn_thread`: two lock sections with the whole of a thread
    /// built between them; `block` is the mapped TLS the build carries across.
    Spawn { pid: Pid, pc: u32, block: Option<u32> },
    /// `process::thread_exit` on a thread that is not the main one.
    ThreadExit { pid: Pid, tid: Tid, code: i32, pc: u32, post: Option<Watch> },
    /// `sys_thread_join`: collect or arm, then re-check.
    Join { pid: Pid, target: Tid, waiter: Tid, pc: u32 },
    /// The idle loop's `reap_poisoned`, reap half.
    IdlePass { pc: u32 },
}

impl Op {
    pub fn exit(pid: Pid, tid: Tid, code: i32) -> Self {
        Op::Exit { pid, tid, code, pc: 0, others: Vec::new(), next: 0 }
    }
    pub fn kill(pid: Pid, code: i32) -> Self {
        Op::Kill { pid, code, pc: 0, tids: Vec::new(), next: 0 }
    }
    pub fn spawn(pid: Pid) -> Self {
        Op::Spawn { pid, pc: 0, block: None }
    }
    pub fn thread_exit(pid: Pid, tid: Tid, code: i32) -> Self {
        Op::ThreadExit { pid, tid, code, pc: 0, post: None }
    }
    pub fn join(pid: Pid, target: Tid, waiter: Tid) -> Self {
        Op::Join { pid, target, waiter, pc: 0 }
    }
    pub fn idle_pass() -> Self {
        Op::IdlePass { pc: 0 }
    }

    fn done(&self) -> bool {
        let (Op::Exit { pc, .. }
        | Op::Kill { pc, .. }
        | Op::Spawn { pc, .. }
        | Op::ThreadExit { pc, .. }
        | Op::Join { pc, .. }
        | Op::IdlePass { pc, .. }) = self;
        *pc == DONE
    }

    fn label(&self) -> &'static str {
        match self {
            Op::Exit { .. } => "exit",
            Op::Kill { .. } => "kill",
            Op::Spawn { .. } => "spawn_thread",
            Op::ThreadExit { .. } => "thread_exit",
            Op::Join { .. } => "thread_join",
            Op::IdlePass { .. } => "idle pass",
        }
    }

    /// Run one lock section.
    fn step(&mut self, world: &mut World) {
        match self {
            Op::Exit { pid, tid, code, pc, others, next } => {
                match *pc {
                    // Phase 1: claim the teardown and collect the threads to
                    // retire, under the table lock.
                    0 => {
                        let present = world.get(*pid).is_some_and(|p| p.location(*tid).is_some());
                        if !present || !teardown::claim_teardown(world, *pid) {
                            *pc = DONE;
                            return;
                        }
                        let set = teardown::exit_set(world.get(*pid).expect("just claimed"), *tid);
                        *others = set.others;
                        // The claimant never returns to Ring 3 — `exit_current`
                        // is where it leaves — so from here it can no more
                        // touch a freed mapping than a retired thread can. It
                        // has released nobody yet, which is the difference and
                        // why this is not `retire`.
                        world.leaving(*pid, *tid);
                        *pc = 1;
                    }
                    // Phase 2: one `retire_task` per other thread, each with
                    // the table lock given up.
                    1 => {
                        if *next < others.len() {
                            world.retire(*pid, others[*next]);
                            *next += 1;
                        } else {
                            *pc = 2;
                        }
                    }
                    // Phase 4: the zombie marks, under the table lock.
                    2 => {
                        if let Some(proc) = world.get_mut(*pid) {
                            teardown::mark_all_zombie(proc, *code);
                        }
                        *pc = 3;
                    }
                    // Phase 5: publish, with the table lock given up.
                    3 => {
                        world.publish_exit(*pid, *code);
                        *pc = 4;
                    }
                    // `exit_current`: the exit pass, one pass later, drops this
                    // thread's payload and `publish_released` posts on its own
                    // watch.
                    _ => {
                        world.retire(*pid, *tid);
                        *pc = DONE;
                    }
                }
            }
            Op::Kill { pid, code, pc, tids, next } => match *pc {
                0 => {
                    if !teardown::claim_teardown(world, *pid) {
                        *pc = DONE;
                        return;
                    }
                    *tids = teardown::kill_set(world.get(*pid).expect("just claimed"));
                    *pc = 1;
                }
                1 => {
                    if *next < tids.len() {
                        world.retire(*pid, tids[*next]);
                        *next += 1;
                    } else {
                        *pc = 2;
                    }
                }
                2 => {
                    if let Some(proc) = world.get_mut(*pid) {
                        teardown::mark_all_zombie(proc, *code);
                    }
                    *pc = 3;
                }
                _ => {
                    world.publish_exit(*pid, *code);
                    *pc = DONE;
                }
            },
            Op::Spawn { pid, pc, block } => match *pc {
                // Phase 1, under the table lock.
                0 => {
                    *pc = if spawn::admit_thread_start(world, *pid).is_yes() { 1 } else { DONE };
                }
                // Phase 2: the TLS block, the mapping, the rebase and the
                // kernel stack — every lock given up, and the whole of the
                // window this op exists to open.
                1 => {
                    *block = Some(world.map_tls());
                    *pc = 2;
                }
                // Phase 3: the insert question, then the table insert and
                // enqueue; a refusal releases the mapping the build carried.
                _ => {
                    let carried = block.expect("phase 2 mapped it");
                    if spawn::admit_thread_insert(world, *pid).is_yes() {
                        world.spawn_thread(*pid);
                        world.adopt_tls(carried);
                    } else {
                        world.release_tls(carried);
                    }
                    *pc = DONE;
                }
            },
            Op::ThreadExit { pid, tid, code, pc, post } => match *pc {
                0 => match teardown::route_thread_exit(world, *pid, *tid) {
                    teardown::ThreadExit::Sibling { post: on } => {
                        *post = Some(on);
                        *pc = 1;
                    }
                    // The entry went under this thread; the zombie mark has
                    // nothing to write and the rest is a sibling's exit.
                    teardown::ThreadExit::Gone { post: on } => {
                        *post = Some(on);
                        *pc = 2;
                    }
                    // The explorer scripts a sibling; a main thread's exit is
                    // `Op::Exit`.
                    teardown::ThreadExit::Process => *pc = DONE,
                },
                // `release_thread`: the mappings go, then the zombie mark under
                // the table lock.
                1 => {
                    world.set_location(*pid, *tid, ThreadLocation::Zombie(*code));
                    *pc = 2;
                }
                // The post, with the table lock given up, before the exit pass
                // — after it this thread does not run again.
                _ => {
                    if let Some(on) = *post {
                        world.post(on);
                    }
                    world.retire(*pid, *tid);
                    *pc = DONE;
                }
            },
            Op::Join { pid, target, waiter, pc } => match *pc {
                0 => match join::collect_zombie(world, *pid, *target) {
                    Ok(Some(_)) | Err(_) => *pc = DONE,
                    Ok(None) => {
                        world.arm(Watch::Thread(*pid, *target), (*pid, *waiter));
                        *pc = 1;
                    }
                },
                // `completion::wait_until` re-checks its predicate after the
                // arm, so a zombie that appeared in the window is collected
                // rather than waited for.
                _ => {
                    if let Ok(Some(_)) = join::collect_zombie(world, *pid, *target) {
                        world.post(Watch::Thread(*pid, *target));
                    }
                    *pc = DONE;
                }
            },
            Op::IdlePass { pc } => {
                for pid in reap::finished_pids(world) {
                    world.reap(pid);
                }
                *pc = DONE;
            }
        }
    }
}

const DONE: u32 = u32::MAX;

/// The first schedule that breaks a law, or `None`.
///
/// Depth-first over "which op runs its next lock section", checking
/// `World::faults` at every state and `World::final_faults` at every leaf. The
/// returned string is the schedule that produced it, in the order the ops ran.
pub fn explore(initial: &World, ops: &[Op]) -> Option<String> {
    let mut trace = Vec::new();
    walk(initial.clone(), ops.to_vec(), &mut trace)
}

fn walk(world: World, ops: Vec<Op>, trace: &mut Vec<String>) -> Option<String> {
    if ops.iter().all(Op::done) {
        let faults = world.final_faults();
        return report(&faults, trace);
    }
    for i in 0..ops.len() {
        if ops[i].done() {
            continue;
        }
        let mut next_world = world.clone();
        let mut next_ops = ops.clone();
        let before = alloc::format!("{}#{i}", next_ops[i].label());
        next_ops[i].step(&mut next_world);
        trace.push(before);
        let faults = next_world.faults();
        if let Some(found) = report(&faults, trace) {
            trace.pop();
            return Some(found);
        }
        if let Some(found) = walk(next_world, next_ops, trace) {
            trace.pop();
            return Some(found);
        }
        trace.pop();
    }
    None
}

fn report(faults: &[String], trace: &[String]) -> Option<String> {
    if faults.is_empty() {
        return None;
    }
    Some(alloc::format!("{}\n  schedule: {}", faults.join("\n"), trace.join(" -> ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The negative control's subject, and #142's shape.** A `SYS_EXIT` on
    /// one thread and a `SYS_THREAD_SPAWN` on another, every ordering: the
    /// spawn's two lock sections have the whole build of a thread between them,
    /// and the exit claims the process in that window.
    ///
    /// Reds under `mutate-spawn-skips-the-insert-recheck`, where the second
    /// question is not asked — the thread lands in the table behind the retire
    /// sweep, its process publishes an exit without it, and the schedule that
    /// did it is printed.
    #[test]
    fn a_published_exit_leaves_no_unretired_thread() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let sibling = world.spawn_thread(pid);
        assert!(
            !world.is_retired(pid, sibling),
            "the sibling starts unretired, which is what the exit's sweep is for",
        );

        let ops = vec![Op::exit(pid, main, 0), Op::spawn(pid)];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// The same window with the teardown coming from outside the process: a
    /// `SYS_PROCESS_KILL` racing a `SYS_THREAD_SPAWN` the target is making.
    #[test]
    fn a_kill_racing_a_spawn_leaves_no_unretired_thread() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let ops = vec![Op::kill(pid, 137), Op::spawn(pid)];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// **The second negative control's subject.** Two teardowns and one
    /// process: a `SYS_EXIT` on the process's own main thread and a
    /// `SYS_PROCESS_KILL` from a handle holder, in every ordering. Exactly one
    /// claim may succeed and exactly one exit may be published —
    /// `World::publish_exit` asserts the second half exactly as
    /// `ProcessObject::publish_exit` does, `World::faults` the first.
    ///
    /// Reds under `mutate-claim-teardown-always-wins`, where both paths retire
    /// the same threads and both publish.
    #[test]
    fn an_exit_and_a_kill_never_both_tear_a_process_down() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        world.spawn_thread(pid);
        let ops = vec![Op::exit(pid, main, 0), Op::kill(pid, 137)];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// A sibling exits while another sibling — not the main thread — is joining
    /// it, in every ordering: the join may arrive before the zombie mark,
    /// between the mark and the post, or after both, and it may arm in any of
    /// those windows.
    #[test]
    fn a_sibling_join_is_answered_however_the_two_interleave() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let waiter = world.spawn_thread(pid);
        let dying = world.spawn_thread(pid);
        let ops = vec![Op::thread_exit(pid, dying, 3), Op::join(pid, dying, waiter)];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// A teardown, an idle pass taking the entry, and a third thread joining a
    /// second one somewhere among them. Nothing may be published twice, no
    /// thread may survive the publish, and the joiner — which the same teardown
    /// is retiring — may not be counted as stranded for waiting on a process
    /// that is taking it with it.
    #[test]
    fn a_reap_racing_a_teardown_and_a_join_breaks_no_law() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let dying = world.spawn_thread(pid);
        let joiner = world.spawn_thread(pid);
        let ops = vec![
            Op::exit(pid, main, 0),
            Op::idle_pass(),
            Op::join(pid, dying, joiner),
        ];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// **The three-way form of #142's window**: a kill, a spawn racing its
    /// sweep, and the idle pass that takes the entry out of the table. Two
    /// operations were what the crate landed with, and the sighting is a machine
    /// where the reaper is a third CPU rather than a phase of one of them.
    #[test]
    fn a_spawn_racing_a_kill_and_the_pass_that_reaps_it() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let ops = vec![Op::kill(pid, 137), Op::spawn(pid), Op::idle_pass()];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// A sibling leaving through `SYS_THREAD_EXIT` while the main thread's own
    /// exit sweeps the process and a third thread is being built.
    ///
    /// The three doors out of a process at once, which is what the T14 log
    /// shows: `/system/bin/ls` spawning while a shell reaps and a terminal exits.
    #[test]
    fn a_sibling_exit_a_spawn_and_the_processs_own_exit() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let sibling = world.spawn_thread(pid);
        let ops = vec![
            Op::exit(pid, main, 0),
            Op::thread_exit(pid, sibling, 3),
            Op::spawn(pid),
        ];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// Two spawns and one teardown: the window admits at most one thread behind
    /// the sweep, and the second question is asked of each of them separately.
    ///
    /// One spawn cannot show that. The insert recheck reads a flag rather than
    /// a count, so a rule that admitted *the first* arrival after the claim and
    /// refused the rest would pass the two-op case and break here.
    #[test]
    fn two_spawns_race_one_teardown() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let ops = vec![Op::exit(pid, main, 0), Op::spawn(pid), Op::spawn(pid)];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// The teeth behind L5: the shipped shape — a refused insert dropping the
    /// built `ThreadData`, freeing the pages and unmapping nothing — is the leak
    /// the law reports. Run by hand, since the shape is no longer the kernel's.
    #[test]
    fn the_refusal_that_dropped_without_unmapping_is_the_leak_l5_reports() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);

        let mut spawning = Op::spawn(pid);
        spawning.step(&mut world); // phase 1: admitted
        spawning.step(&mut world); // phase 2: the block is mapped

        let mut exit = Op::exit(pid, main, 0);
        while !exit.done() {
            exit.step(&mut world);
        }
        assert!(
            !spawn::admit_thread_insert(&world, pid).is_yes(),
            "the claimed teardown must refuse the insert, or this stages nothing",
        );
        // The old shape: return None with the mapping still in the local.

        let faults = world.final_faults();
        assert!(
            faults.iter().any(|f| f.contains("without unmapping")),
            "L5 cannot see the dropped mapping, so the schedules above pass vacuously: {faults:?}",
        );
    }

    /// A `SYS_THREAD_JOIN` armed on a thread the killer is about to retire, in
    /// every ordering — the waiter that L4 is about and the one shape where it
    /// is answered by a teardown rather than by the thread it named.
    #[test]
    fn a_join_racing_the_kill_that_takes_its_target() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let target = world.spawn_thread(pid);
        let waiter = world.spawn_thread(pid);
        let ops = vec![Op::kill(pid, 137), Op::join(pid, target, waiter)];
        if let Some(found) = explore(&world, &ops) {
            panic!("a lifecycle law broke:\n{found}");
        }
    }

    /// **A thread whose entry went under it still finishes its own exit**: a
    /// kill publishes, an idle pass takes the entry, and a sibling already on
    /// its way into `SYS_THREAD_EXIT` arrives after both.
    #[test]
    fn a_thread_exit_that_outlived_its_entry_still_leaves() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let sibling = world.spawn_thread(pid);

        // The schedule, run by hand rather than searched for: a kill that runs
        // to its publish, an idle pass, and only then the sibling's own exit.
        let mut kill = Op::kill(pid, 137);
        while !kill.done() {
            kill.step(&mut world);
        }
        let mut idle = Op::idle_pass();
        idle.step(&mut world);
        assert!(world.was_reaped(pid), "the idle pass took the published entry");

        assert_eq!(
            teardown::route_thread_exit(&world, pid, sibling),
            teardown::ThreadExit::Gone { post: Watch::Thread(pid, sibling) },
            "a thread whose process was reaped under it leaves by the sibling door",
        );

        // The route is not the end of the exit: a thread routed to a state its
        // caller does not survive stops here, and one out of the sibling door
        // still has its post and its retire to run.
        let mut exit = Op::thread_exit(pid, sibling, 0);
        exit.step(&mut world);
        assert!(
            !exit.done(),
            "the exit ended at its routing section: a thread whose entry went under it \
             still has to post and retire, and one that stops here is the machine \
             stopping with it",
        );
        while !exit.done() {
            exit.step(&mut world);
        }
    }
}
