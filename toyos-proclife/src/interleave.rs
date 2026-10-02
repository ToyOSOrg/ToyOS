//! Every ordering of two CPUs inside one process's lifecycle.
//!
//! **This is the file the crate exists for.** `kernel/src/process.rs` gives the
//! process table lock up between every phase of a teardown and between both
//! halves of a spawn, and its comments say what each window costs — "a thread
//! enqueued now would be invisible to its retire sweep", "once it is published
//! the entry is reapable, so nothing may read the table for this pid after this
//! point". Not
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
use std::collections::HashSet;

use crate::model::{Climb, World};
use crate::table::Processes;
use crate::tree::{self, Admit, Admitted};
use crate::{join, reap, spawn, teardown, Pid, Tid, Watch};

/// `process::KILLED_EXIT_CODE`, which a walk claims every process below an end
/// with.
const KILLED: i32 = 137;

/// One kernel path, mid-flight.
///
/// Each variant's `pc` is the number of lock sections it has completed, and
/// every field beside it is a value the real path carries in a local across a
/// lock release — which is the whole reason a window exists to explore. A
/// thread's own way out after its operation is the [`World`]'s: `depart_step`.
///
/// An exit's and a kill's `owed` is its walk: the processes below its end not
/// yet claimed, one claim per lock section, each claim's retires posted with
/// the lock given up.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// `process::exit`: a thread ending its own process — claim, retire the
    /// rest, walk the subtree, leave.
    Exit { pid: Pid, tid: Tid, code: i32, pc: u32, retire: Vec<(Pid, Tid)>, owed: Vec<Pid> },
    /// `process::kill_process`: claim, retire every thread, walk the subtree,
    /// return. `by` is the killing thread when the model holds it.
    Kill { pid: Pid, code: i32, pc: u32, retire: Vec<(Pid, Tid)>, owed: Vec<Pid>, by: Option<(Pid, Tid)> },
    /// `loader::spawn` under `place` by `by`'s thread: the admission, the
    /// whole of a process built with every lock given up, then the move of
    /// the caller's handles and the landing, whose retires the landing
    /// answers. A build that `fails` lets the place go instead, and climbs
    /// when that was the place's last hold.
    SpawnUnder {
        place: Pid,
        by: (Pid, Tid),
        fails: bool,
        pc: u32,
        admitted: Option<Admitted>,
        retire: Vec<(Pid, Tid)>,
        climb: Option<Climb>,
    },
    /// `process::spawn_thread`: two lock sections with the whole of a thread
    /// built between them; `block` is the mapped TLS the build carries across.
    Spawn { pid: Pid, pc: u32, block: Option<u32> },
    /// `process::thread_exit` on a thread that is not the main one.
    ThreadExit { pid: Pid, tid: Tid, code: i32, pc: u32 },
    /// `sys_thread_join`: collect or arm, then re-check.
    Join { pid: Pid, target: Tid, waiter: Tid, pc: u32 },
    /// The idle loop's `reap_finished`.
    IdlePass { pc: u32 },
}

impl Op {
    pub fn exit(pid: Pid, tid: Tid, code: i32) -> Self {
        Op::Exit { pid, tid, code, pc: 0, retire: Vec::new(), owed: Vec::new() }
    }
    pub fn kill(pid: Pid, code: i32) -> Self {
        Op::Kill { pid, code, pc: 0, retire: Vec::new(), owed: Vec::new(), by: None }
    }
    /// A kill issued by a thread the model holds, which is in the kernel until
    /// the kill returns.
    pub fn kill_by(pid: Pid, code: i32, by: (Pid, Tid)) -> Self {
        Op::Kill { pid, code, pc: 0, retire: Vec::new(), owed: Vec::new(), by: Some(by) }
    }
    /// A spawn under `place` by `by`'s thread, which is in the kernel until it
    /// returns.
    pub fn spawn_under(place: Pid, by: (Pid, Tid)) -> Self {
        Op::SpawnUnder { place, by, fails: false, pc: 0, admitted: None, retire: Vec::new(), climb: None }
    }
    /// The same spawn, whose build fails once it is admitted.
    pub fn spawn_under_failing(place: Pid, by: (Pid, Tid)) -> Self {
        Op::SpawnUnder { place, by, fails: true, pc: 0, admitted: None, retire: Vec::new(), climb: None }
    }
    pub fn spawn(pid: Pid) -> Self {
        Op::Spawn { pid, pc: 0, block: None }
    }
    pub fn thread_exit(pid: Pid, tid: Tid, code: i32) -> Self {
        Op::ThreadExit { pid, tid, code, pc: 0 }
    }
    pub fn join(pid: Pid, target: Tid, waiter: Tid) -> Self {
        Op::Join { pid, target, waiter, pc: 0 }
    }
    pub fn idle_pass() -> Self {
        Op::IdlePass { pc: 0 }
    }

    /// The thread running the op, in the kernel until it returns.
    fn actor(&self) -> Option<(Pid, Tid)> {
        match *self {
            Op::Exit { pid, tid, .. } | Op::ThreadExit { pid, tid, .. } => Some((pid, tid)),
            Op::Join { pid, waiter, .. } => Some((pid, waiter)),
            Op::Kill { by, .. } => by,
            Op::SpawnUnder { by, .. } => Some(by),
            Op::Spawn { .. } | Op::IdlePass { .. } => None,
        }
    }

    fn done(&self) -> bool {
        match self {
            Op::Exit { pc, .. }
            | Op::Kill { pc, .. }
            | Op::SpawnUnder { pc, .. }
            | Op::Spawn { pc, .. }
            | Op::ThreadExit { pc, .. }
            | Op::Join { pc, .. }
            | Op::IdlePass { pc, .. } => *pc == DONE,
        }
    }

    /// The process whose teardown the op is waiting on, if its next section
    /// cannot run until that process publishes its exit.
    fn awaits_teardown(&self, world: &World) -> Option<Pid> {
        match *self {
            Op::Kill { pid, pc: WAIT, .. } if world.published(pid).is_none() => Some(pid),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Op::Exit { .. } => "exit",
            Op::Kill { .. } => "kill",
            Op::SpawnUnder { .. } => "spawn_under",
            Op::Spawn { .. } => "spawn_thread",
            Op::ThreadExit { .. } => "thread_exit",
            Op::Join { .. } => "thread_join",
            Op::IdlePass { .. } => "idle pass",
        }
    }

    /// Run one lock section.
    fn step(&mut self, world: &mut World) {
        match self {
            Op::Exit { pid, tid, code, pc, retire, owed } => match *pc {
                // The claim, the threads its winner retires and the children
                // it owes, under the table lock.
                0 => {
                    if tree::claim(world, *pid, *code, owed) {
                        let proc = world.get(*pid).expect("just claimed");
                        *retire = teardown::retire_set(proc, Some(*tid)).into_iter().map(|t| (*pid, t)).collect();
                    }
                    *pc = POST;
                }
                // With the lock given up: the retires, then the walk's next
                // claim, or this thread's own way out.
                POST => {
                    for (victim, thread) in retire.drain(..) {
                        world.post_retire(victim, thread);
                    }
                    if owed.is_empty() {
                        world.depart(*pid, *tid, None);
                        *pc = DONE;
                    } else {
                        *pc = WALK;
                    }
                }
                _ => {
                    walk_claim(world, owed, retire);
                    *pc = POST;
                }
            },
            Op::Kill { pid, code, pc, retire, owed, by } => {
                match *pc {
                    0 => {
                        if tree::claim(world, *pid, *code, owed) {
                            let proc = world.get(*pid).expect("just claimed");
                            *retire = teardown::retire_set(proc, None).into_iter().map(|t| (*pid, t)).collect();
                            *pc = POST;
                        } else {
                            *pc = DONE;
                        }
                    }
                    POST => {
                        for (victim, thread) in retire.drain(..) {
                            world.post_retire(victim, thread);
                        }
                        *pc = match owed.is_empty() {
                            false => WALK,
                            true if cfg!(feature = "mutate-kill-waits-for-its-victims") => WAIT,
                            true => DONE,
                        };
                    }
                    WALK => {
                        walk_claim(world, owed, retire);
                        *pc = POST;
                    }
                    // The mutation's wait, which `enabled` holds until the victim's exit is published.
                    _ => *pc = DONE,
                }
                if *pc == DONE {
                    if let Some(by) = *by {
                        world.leave_kernel(by);
                    }
                }
            }
            Op::SpawnUnder { place, by, fails, pc, admitted, retire, climb } => {
                match *pc {
                    // The admission, under the table lock and before anything is built.
                    0 => match tree::admit_child(world, Some(*place)) {
                        Admit::Yes(taken) => {
                            *admitted = Some(taken);
                            *pc = 1;
                        }
                        Admit::Gone | Admit::TooDeep { .. } | Admit::NoPid => {
                            world.refuse_spawn(*by);
                            *pc = DONE;
                        }
                    },
                    // The build happened with every lock given up, and failed.
                    1 if *fails => {
                        let taken = admitted.take().expect("admitted at the first section");
                        world.refuse_spawn(*by);
                        match tree::refuse_child(world, taken) {
                            Some(publish) => {
                                *climb = Some(Climb::Publish(publish));
                                *pc = 3;
                            }
                            None => *pc = DONE,
                        }
                    }
                    // The caller's handles move, and the child lands, in the
                    // hold that inserts it.
                    1 => {
                        world.move_handles(*by);
                        let taken = admitted.take().expect("admitted at the first section");
                        let child = taken.pid();
                        let ((), owed) =
                            tree::land_child(world, taken, KILLED, |world, node| world.insert(child, node));
                        world.landed(*place, child);
                        *retire = owed.into_iter().map(|t| (child, t)).collect();
                        *pc = 2;
                    }
                    // With the lock given up: the retires the landing answered.
                    2 => {
                        for (victim, thread) in retire.drain(..) {
                            world.post_retire(victim, thread);
                        }
                        *pc = DONE;
                    }
                    // The failed build let its place's last hold go: this
                    // spawner publishes it, and climbs.
                    _ => {
                        let at = climb.take().expect("a climb is owed here");
                        *climb = world.climb_step(at);
                        if climb.is_none() {
                            *pc = DONE;
                        }
                    }
                }
                if *pc == DONE {
                    world.leave_kernel(*by);
                }
            }
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
            Op::ThreadExit { pid, tid, code, pc } => match *pc {
                0 => match teardown::route_thread_exit(world, *pid, *tid) {
                    teardown::ThreadExit::Sibling { .. } => *pc = 1,
                    // The explorer scripts a sibling; a main thread's exit is
                    // `Op::Exit`.
                    teardown::ThreadExit::Process => *pc = DONE,
                },
                // `release_thread`'s unmap, then this thread's own way out.
                _ => {
                    world.unmap_own(*pid, *tid);
                    world.depart(*pid, *tid, Some(*code));
                    *pc = DONE;
                }
            },
            Op::Join { pid, target, waiter, pc } => {
                match *pc {
                    0 => match join::collect_zombie(world, *pid, *target) {
                        Ok(Some(_)) => {
                            world.drop_collected(*pid, *target);
                            *pc = DONE;
                        }
                        Err(_) => *pc = DONE,
                        Ok(None) => {
                            world.arm(Watch::Thread(*pid, *target), (*pid, *waiter));
                            *pc = 1;
                        }
                    },
                    // `watch::wait_until` re-checks its predicate after the
                    // arm, so a zombie that appeared in the window is collected
                    // rather than waited for.
                    _ => {
                        if let Ok(Some(_)) = join::collect_zombie(world, *pid, *target) {
                            world.drop_collected(*pid, *target);
                            world.post(Watch::Thread(*pid, *target));
                        }
                        *pc = DONE;
                    }
                }
                if *pc == DONE {
                    world.leave_kernel((*pid, *waiter));
                }
            }
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
/// An exit's or a kill's section that posts the retires its last claim owes.
const POST: u32 = 1;
/// The walk's next claim.
const WALK: u32 = 2;
/// `mutate-kill-waits-for-its-victims`' wait.
const WAIT: u32 = 3;

/// The walk's next claim, under the table lock: one process, its retires and
/// the children it owes. Under `mutate-walk-in-one-hold`, every process the
/// walk owes, in this one section.
fn walk_claim(world: &mut World, owed: &mut Vec<Pid>, retire: &mut Vec<(Pid, Tid)>) {
    loop {
        let Some(next) = owed.pop() else { return };
        if tree::claim(world, next, KILLED, owed) {
            let proc = world.get(next).expect("just claimed");
            retire.extend(teardown::retire_set(proc, None).into_iter().map(|t| (next, t)));
        }
        if !cfg!(feature = "mutate-walk-in-one-hold") {
            return;
        }
    }
}

/// One move the explorer can make: an op's next section, or a thread's next
/// step out.
#[derive(Clone, Copy)]
enum Move {
    Op(usize),
    Out(Pid, Tid),
}

/// How many distinct states every schedule reaches, or the first schedule that breaks a law.
///
/// Depth-first over "which op runs its next lock section, or which thread
/// takes its next step out", checking `World::faults` and that no op waits on
/// another process's teardown at every state, and `World::final_faults` at
/// every leaf; a state where nothing can move is a deadlock. A state reached
/// before is not walked again: every law is a property of the state alone. The
/// returned string is the schedule that produced it, in the order it ran.
pub fn explore(initial: &World, ops: &[Op]) -> Result<usize, String> {
    explore_leaves(initial, ops, &mut |_| {})
}

/// [`explore`], answering whether some schedule ends in a world `pred` holds
/// of: a property one ordering has, where [`explore`]'s laws are ones every
/// ordering keeps.
pub fn explore_any(initial: &World, ops: &[Op], pred: impl Fn(&World) -> bool) -> Result<bool, String> {
    let mut found = false;
    explore_leaves(initial, ops, &mut |leaf| found |= pred(leaf))?;
    Ok(found)
}

fn explore_leaves(initial: &World, ops: &[Op], leaf: &mut dyn FnMut(&World)) -> Result<usize, String> {
    let mut world = initial.clone();
    for op in ops {
        if let Some(actor) = op.actor() {
            world.enter_kernel(actor);
        }
    }
    let mut trace = Vec::new();
    let mut seen = HashSet::new();
    match walk(world, ops.to_vec(), &mut trace, &mut seen, leaf) {
        Some(found) => Err(found),
        None => Ok(seen.len()),
    }
}

fn walk(
    world: World,
    ops: Vec<Op>,
    trace: &mut Vec<String>,
    seen: &mut HashSet<(World, Vec<Op>)>,
    leaf: &mut dyn FnMut(&World),
) -> Option<String> {
    if !seen.insert((world.clone(), ops.clone())) {
        return None;
    }
    let mut moves: Vec<Move> = (0..ops.len())
        .filter(|&i| !ops[i].done() && ops[i].awaits_teardown(&world).is_none())
        .map(Move::Op)
        .collect();
    moves.extend(world.ready_departures().into_iter().map(|(pid, tid)| Move::Out(pid, tid)));
    if moves.is_empty() {
        let stuck: Vec<&str> = ops.iter().filter(|op| !op.done()).map(Op::label).collect();
        if stuck.is_empty() {
            leaf(&world);
            return report(&world.final_faults(), trace);
        }
        return report(&[alloc::format!("deadlock: {} each wait and none can move", stuck.join(", "))], trace);
    }
    for m in moves {
        let mut next_world = world.clone();
        let mut next_ops = ops.clone();
        let label = match m {
            Move::Op(i) => {
                let label = alloc::format!("{}#{i}", next_ops[i].label());
                next_ops[i].step(&mut next_world);
                label
            }
            Move::Out(pid, tid) => {
                next_world.depart_step(pid, tid);
                alloc::format!("out {pid}/{tid}")
            }
        };
        trace.push(label);
        let mut faults = next_world.faults();
        // L7. No kill or exit waits on another process's teardown.
        for op in &next_ops {
            if let Some(pid) = op.awaits_teardown(&next_world) {
                if op.actor().map(|(own, _)| own) != Some(pid) {
                    faults.push(alloc::format!("a {} waits on pid {pid}'s teardown", op.label()));
                }
            }
        }
        if let Some(found) = report(&faults, trace).or_else(|| walk(next_world, next_ops, trace, seen, leaf)) {
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

    fn holds(world: &World, ops: Vec<Op>) -> usize {
        match explore(world, &ops) {
            Ok(states) => states,
            Err(found) => panic!("a lifecycle law broke:\n{found}"),
        }
    }

    /// **The negative control's subject, and #142's shape.** A `SYS_EXIT` on
    /// one thread and a `SYS_THREAD_SPAWN` on another, every ordering: the
    /// spawn's two lock sections have the whole build of a thread between them,
    /// and the exit claims the process in that window.
    ///
    /// Reds under `mutate-spawn-skips-the-insert-recheck`, where the second
    /// question is not asked — the thread lands in the table behind the retire
    /// set, nothing retires it, and its process never ends.
    #[test]
    fn a_published_exit_leaves_no_unretired_thread() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        world.spawn_thread(pid);
        holds(&world, vec![Op::exit(pid, main, 0), Op::spawn(pid)]);
    }

    /// The same window with the teardown coming from outside the process: a
    /// `SYS_PROCESS_KILL` racing a `SYS_THREAD_SPAWN` the target is making.
    #[test]
    fn a_kill_racing_a_spawn_leaves_no_unretired_thread() {
        let mut world = World::new();
        let pid = world.spawn_process();
        holds(&world, vec![Op::kill(pid, 137), Op::spawn(pid)]);
    }

    /// **The second negative control's subject.** Two teardowns and one
    /// process: a `SYS_EXIT` on the process's own main thread and a
    /// `SYS_PROCESS_KILL` from a handle holder, in every ordering. Exactly one
    /// claim may succeed, one teardown run and one exit be published —
    /// `World::publish_exit` asserts the last exactly as
    /// `ProcessObject::publish_exit` does, `World::faults` the first two.
    ///
    /// Reds under `mutate-claim-teardown-always-wins`, where both paths retire
    /// the same threads, and under `mutate-first-out-tears-down`, where the
    /// first thread out frees what its sibling still runs in.
    #[test]
    fn an_exit_and_a_kill_never_both_tear_a_process_down() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        world.spawn_thread(pid);
        holds(&world, vec![Op::exit(pid, main, 0), Op::kill(pid, 137)]);
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
        holds(&world, vec![Op::thread_exit(pid, dying, 3), Op::join(pid, dying, waiter)]);
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
        holds(&world, vec![Op::exit(pid, main, 0), Op::idle_pass(), Op::join(pid, dying, joiner)]);
    }

    /// **The three-way form of #142's window**: a kill, a spawn racing its
    /// retire set, and the idle pass that takes the entry out of the table.
    #[test]
    fn a_spawn_racing_a_kill_and_the_pass_that_reaps_it() {
        let mut world = World::new();
        let pid = world.spawn_process();
        holds(&world, vec![Op::kill(pid, 137), Op::spawn(pid), Op::idle_pass()]);
    }

    /// A sibling leaving through `SYS_THREAD_EXIT` while the main thread's own
    /// exit claims the process and a third thread is being built.
    ///
    /// The three doors out of a process at once, which is what the T14 log
    /// shows: `/system/bin/ls` spawning while a shell reaps and a terminal exits.
    #[test]
    fn a_sibling_exit_a_spawn_and_the_processs_own_exit() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let sibling = world.spawn_thread(pid);
        holds(&world, vec![Op::exit(pid, main, 0), Op::thread_exit(pid, sibling, 3), Op::spawn(pid)]);
    }

    /// Two spawns and one teardown: the window admits at most one thread behind
    /// the claim, and the second question is asked of each of them separately.
    ///
    /// One spawn cannot show that. The insert recheck reads a flag rather than
    /// a count, so a rule that admitted *the first* arrival after the claim and
    /// refused the rest would pass the two-op case and break here.
    #[test]
    fn two_spawns_race_one_teardown() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        holds(&world, vec![Op::exit(pid, main, 0), Op::spawn(pid), Op::spawn(pid)]);
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

    /// A `SYS_THREAD_JOIN` armed on a thread the kill is about to retire, in
    /// every ordering — the waiter that L4 is about, and a joiner that must not
    /// take its target's entry while a sibling still runs in its mappings.
    ///
    /// Reds under `mutate-join-collects-in-a-teardown`.
    #[test]
    fn a_join_racing_the_kill_that_takes_its_target() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let target = world.spawn_thread(pid);
        let waiter = world.spawn_thread(pid);
        holds(&world, vec![Op::kill(pid, 137), Op::join(pid, target, waiter)]);
    }

    /// **`KillEachOther`, K1's shape**: two processes, each one's thread inside
    /// `SYS_PROCESS_KILL` on the other. A thread in the kernel reaches no safe
    /// point until its kill returns, so a kill that waited for its victim waits
    /// on a process that is waiting on it. Each process has a second thread,
    /// and one of the killers is it.
    ///
    /// Reds under `mutate-kill-waits-for-its-victims`.
    #[test]
    fn two_processes_killing_each_other_both_end() {
        let mut world = World::new();
        let p = world.spawn_process();
        let p_sibling = world.spawn_thread(p);
        let c = world.spawn_process();
        world.spawn_thread(c);
        let states = holds(
            &world,
            vec![Op::kill_by(c, 137, (p, p_sibling)), Op::kill_by(p, 137, (c, world.main_tid(c)))],
        );
        std::println!("KillEachOther: {states} states, every schedule ends");
    }

    /// The cycle at length three: A kills B, B kills C, C kills A.
    #[test]
    fn a_kill_chain_of_three_ends() {
        let mut world = World::new();
        let a = world.spawn_process();
        let b = world.spawn_process();
        let c = world.spawn_process();
        holds(
            &world,
            vec![
                Op::kill_by(b, 137, (a, world.main_tid(a))),
                Op::kill_by(c, 137, (b, world.main_tid(b))),
                Op::kill_by(a, 137, (c, world.main_tid(c))),
            ],
        );
    }

    /// A sibling in the cycle and an exit racing it: P's main thread exits
    /// while P's second thread kills C and C kills P.
    #[test]
    fn an_exit_whose_sibling_kills_the_process_killing_it() {
        let mut world = World::new();
        let p = world.spawn_process();
        let killer = world.spawn_thread(p);
        let c = world.spawn_process();
        holds(
            &world,
            vec![
                Op::exit(p, world.main_tid(p), 0),
                Op::kill_by(c, 137, (p, killer)),
                Op::kill_by(p, 137, (c, world.main_tid(c))),
            ],
        );
    }

    /// A process's one thread exits: it claims, leaves and tears the process
    /// down, and it is in the process for all of the teardown.
    ///
    /// Reds under `mutate-last-out-leaves-before-its-teardown`.
    #[test]
    fn the_last_one_out_is_in_its_process_until_its_teardown_is_done() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        holds(&world, vec![Op::exit(pid, main, 0)]);
    }

    /// A thread killing its own process, with a sibling: it retires itself and
    /// leaves when its kill returns.
    #[test]
    fn a_process_that_kills_itself_ends() {
        let mut world = World::new();
        let p = world.spawn_process();
        let sibling = world.spawn_thread(p);
        holds(&world, vec![Op::kill_by(p, 137, (p, sibling))]);
    }

    /// **A spawn racing its place's kill**, every ordering, with the spawner a
    /// process that holds the place's `self` — init, serving a launch — and
    /// its build landing or failing. A child lands before the claim and the
    /// walk takes it, or after it and is claimed as it lands; a failed build
    /// lets the place go. Nothing runs on under the place, a refused spawn
    /// moved none of its caller's handles, and the place is published in
    /// every one.
    ///
    /// Reds under `mutate-place-skips-the-insert-recheck`, where a child lands
    /// unclaimed under the claimed place and outlives it, and under
    /// `mutate-refused-spawn-keeps-the-count`, where the place is never
    /// published.
    #[test]
    fn a_spawn_racing_its_places_kill_leaves_nothing_under_it_and_publishes_it() {
        let mut world = World::new();
        let init = world.spawn_process();
        let place = world.spawn_child(init);
        world.spawn_child(place);
        let launcher = (init, world.main_tid(init));
        let states = holds(&world, vec![Op::kill(place, KILLED), Op::spawn_under(place, launcher)]);
        std::println!("a spawn racing its place's kill: {states} states");
        holds(&world, vec![Op::kill(place, KILLED), Op::spawn_under_failing(place, launcher)]);
    }

    /// The same race with the place spawning under itself: its own thread is
    /// in the spawn when the kill claims it, and its teardown waits for that
    /// thread to come out.
    #[test]
    fn a_spawn_racing_the_kill_of_its_own_spawner() {
        let mut world = World::new();
        let init = world.spawn_process();
        let place = world.spawn_child(init);
        world.spawn_child(place);
        let own = (place, world.main_tid(place));
        holds(&world, vec![Op::kill(place, KILLED), Op::spawn_under(place, own)]);
        holds(&world, vec![Op::kill(place, KILLED), Op::spawn_under_failing(place, own)]);
    }

    /// An exit takes a subtree two deep below it, and each end is published
    /// after every end below it.
    ///
    /// Reds under `mutate-publish-before-the-children`.
    #[test]
    fn an_exit_publishes_after_every_end_below_it() {
        let mut world = World::new();
        let init = world.spawn_process();
        let p = world.spawn_child(init);
        let c = world.spawn_child(p);
        world.spawn_child(c);
        world.spawn_child(p);
        let main = world.main_tid(p);
        holds(&world, vec![Op::exit(p, main, 0)]);
    }

    /// A child's own exit racing its parent's kill: two walks over one
    /// subtree, one claim per process whichever walk makes it.
    #[test]
    fn a_childs_exit_racing_its_parents_kill() {
        let mut world = World::new();
        let init = world.spawn_process();
        let p = world.spawn_child(init);
        let c = world.spawn_child(p);
        world.spawn_child(c);
        let main = world.main_tid(c);
        holds(&world, vec![Op::kill(p, KILLED), Op::exit(c, main, 3)]);
    }

    /// **The walk gives the table up between two claims.** A spawn under a
    /// process the walk does not reach lands, in some ordering, after the
    /// walk's first claim and before its last: an end of any size holds the
    /// table one process at a time.
    ///
    /// Reds under `mutate-walk-in-one-hold`.
    #[test]
    fn a_spawn_under_an_unrelated_process_lands_between_two_claims_of_one_walk() {
        let mut world = World::new();
        let init = world.spawn_process();
        let p = world.spawn_child(init);
        world.spawn_child(p);
        world.spawn_child(p);
        let unrelated = world.spawn_child(init);
        let launcher = (init, world.main_tid(init));
        let ops = vec![Op::kill(p, KILLED), Op::spawn_under(unrelated, launcher)];
        // Two claimed when it landed: the end's own, and one of the two below it.
        let between = match explore_any(&world, &ops, |leaf| {
            leaf.inserted_at().iter().any(|&(_, claimed)| claimed == 2)
        }) {
            Ok(between) => between,
            Err(found) => panic!("a lifecycle law broke:\n{found}"),
        };
        assert!(
            between,
            "no ordering landed the unrelated spawn between the walk's two claims below the end: \
             the walk holds the table across its claims",
        );
    }
}
