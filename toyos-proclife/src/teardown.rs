//! Who ends a process, which threads they must retire, and what a thread's own
//! exit is.
//!
//! **Exactly one path publishes exactly one exit.** Three of them can arrive at
//! once — a `SYS_EXIT` on the process's own main thread, a `SYS_PROCESS_KILL`
//! from a holder of a `Process` handle, and the idle loop's sweep of threads
//! that died in panic recovery — and the whole arrangement rests on
//! [`claim_teardown`] answering `true` to one of them. A second publish is an
//! assertion failure in `ProcessObject::publish_exit`, by design: it means two
//! teardowns claimed one process, and a kernel that tolerated it would free one
//! address space twice.
//!
//! **The claimant retires every other thread before it frees anything.** A
//! thread that is still schedulable when its process's mappings go writes
//! through stale page tables into 2 MiB frames the PMM has already re-issued,
//! so the order — claim, collect, retire, free, mark, publish — is the whole of
//! the teardown's soundness. What this module owns is the *decisions* in that
//! order: which threads are in the set, where each one's CPU time is charged,
//! what code each is marked with. The retire itself is `toyos-sched`'s and the
//! free is the kernel's.

use alloc::vec::Vec;

use crate::table::{Lifecycle, Processes};
use crate::{Pid, ThreadLocation, Tid, TORN_DOWN_THREAD_CODE};

/// Claim exclusive teardown of a process.
///
/// Exactly one exit/kill/poison path wins; a later caller must simply exit its
/// own thread — the claimant's retire sweep handles it like any other thread.
/// `false` also covers a process that is not in the table at all, because there
/// is nothing left for a second claimant to do either way.
#[must_use = "a caller that did not win the claim must not tear anything down"]
pub fn claim_teardown<T: Processes>(table: &mut T, pid: Pid) -> bool {
    let Some(proc) = table.get_mut(pid) else { return false };
    // The mutation this feature stages is the whole of the exclusion: the flag
    // is still raised and still readable, and every arrival is still told it
    // may proceed.
    #[cfg(not(feature = "mutate-claim-teardown-always-wins"))]
    if proc.tearing_down() {
        return false;
    }
    proc.begin_teardown();
    true
}

/// The threads a process's own exit must retire, and whether the thread running
/// that exit is the main one.
///
/// The current thread is **not** in `others`, and cannot be: it is executing
/// the teardown, and `retire_task` returns only when its subject is provably
/// off every CPU. Its own CPU time is read separately, which is what
/// [`ExitSet::current_is_main`] is for — a main thread filtered out of the
/// retire set would otherwise leave `cpu=0ms` on its own exit line.
pub struct ExitSet {
    /// Every thread of the process except the one calling. Sorted, so the
    /// retire order does not depend on a hash seed.
    pub others: Vec<Tid>,
    /// Whether the calling thread is this process's main thread.
    pub current_is_main: bool,
}

pub fn exit_set<P: Lifecycle>(proc: &P, current: Tid) -> ExitSet {
    let mut others = Vec::new();
    proc.each_thread(&mut |tid, _| {
        if tid != current {
            others.push(tid);
        }
    });
    others.sort_unstable();
    ExitSet { others, current_is_main: proc.main_tid() == current }
}

/// Every thread of a process being killed from outside, in retire order.
///
/// Unlike [`exit_set`] this includes the main thread and holds no exception:
/// every thread belongs to another process, so none of them is the one running
/// the kill.
pub fn kill_set<P: Lifecycle>(proc: &P) -> Vec<Tid> {
    let mut tids = Vec::new();
    proc.each_thread(&mut |tid, _| tids.push(tid));
    tids.sort_unstable();
    tids
}

/// Where a retired thread's CPU time is charged.
///
/// The two are not interchangeable: the main thread's is what a process's exit
/// line reports and what `ProcessStats::cpu_ns` is built from, while a
/// sibling's is folded into `child_threads_cpu_ns` and added to it. Charging
/// one as the other double-counts or loses the whole of a process's CPU time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CpuCharge {
    /// The process's own `cpu_ns`.
    MainThread,
    /// `ProcessAccounting::child_threads_cpu_ns`.
    ChildThreads,
}

pub fn charge(tid: Tid, main_tid: Tid) -> CpuCharge {
    if tid == main_tid {
        CpuCharge::MainThread
    } else {
        CpuCharge::ChildThreads
    }
}

/// Mark one thread dead.
///
/// Idempotent, and silent about an entry that has gone: a main thread reaches
/// this after its own process published its exit, by which point any idle pass
/// may already have reaped the entry. A thread already dead keeps the code it
/// died with — the second mark is a teardown arriving behind the thread's own
/// exit, and the code the thread chose is the true one.
pub fn mark_zombie<T: Processes>(table: &mut T, pid: Pid, tid: Tid, code: i32) {
    let Some(proc) = table.get_mut(pid) else { return };
    if proc.location(tid).is_some_and(|l| !l.is_zombie()) {
        proc.set_location(tid, ThreadLocation::Zombie(code));
    }
}

/// Mark every thread of a terminating process dead: the main thread with the
/// process's code, every sibling with [`TORN_DOWN_THREAD_CODE`].
///
/// Runs under the table lock at the end of a teardown, after every thread has
/// been retired — so nothing it marks can still be running, and a thread that
/// is already a zombie has already answered for itself and keeps its code.
pub fn mark_all_zombie<P: Lifecycle>(proc: &mut P, code: i32) {
    let main_tid = proc.main_tid();
    let mut pending: Vec<(Tid, i32)> = Vec::new();
    proc.each_thread(&mut |tid, at| {
        if !at.is_zombie() {
            pending.push((tid, if tid == main_tid { code } else { TORN_DOWN_THREAD_CODE }));
        }
    });
    for (tid, code) in pending {
        proc.set_location(tid, ThreadLocation::Zombie(code));
    }
}

/// Which of the two exits a `SYS_THREAD_EXIT` is.
#[must_use = "a thread exit that is not routed is a thread that never dies"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadExit {
    /// The main thread: this is the process's exit, and the whole teardown
    /// runs.
    Process,
    /// A sibling: release its own mappings and mark it `Zombie(code)`. The exit
    /// pass then drops its payload, and `publish_released`'s post on the
    /// thread's own watch is the one place its death reaches a joiner — a
    /// joiner therefore never runs before the payload is gone.
    Sibling,
    /// The process has no entry — another CPU's kill reaped it while this
    /// thread was on its way here. **The same exit a sibling takes**: nothing
    /// here is the main thread any more, so the teardown branch is skipped and
    /// every table write is a no-op.
    Gone,
}

/// Route a thread's own exit.
///
/// **A missing entry is [`ThreadExit::Gone`] and not a panic**, which is
/// [`mark_zombie`]'s rule one function along: a thread that arrives to find
/// nothing has to leave, not take the machine with it.
pub fn route_thread_exit<T: Processes>(table: &T, pid: Pid, tid: Tid) -> ThreadExit {
    let Some(proc) = table.get(pid) else {
        return ThreadExit::Gone;
    };
    if proc.main_tid() == tid {
        return ThreadExit::Process;
    }
    ThreadExit::Sibling
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;
    use crate::Watch;

    #[cfg(not(feature = "mutate-claim-teardown-always-wins"))]
    #[test]
    fn exactly_one_claimant_wins_however_many_arrive() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(claim_teardown(&mut world, pid));
        assert!(!claim_teardown(&mut world, pid));
        assert!(!claim_teardown(&mut world, pid));
    }

    /// Teeth for the control itself: under the mutation a second claimant
    /// really does win, so a red elsewhere is this revert and not something
    /// else.
    #[cfg(feature = "mutate-claim-teardown-always-wins")]
    #[test]
    fn the_mutation_really_grants_a_second_claim() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(claim_teardown(&mut world, pid));
        assert!(
            claim_teardown(&mut world, pid),
            "the control is inert: the claim is still exclusive, so whatever the \
             model reds on under it is not this mutation",
        );
    }

    #[test]
    fn a_process_that_is_gone_grants_no_claim() {
        let mut world = World::new();
        assert!(!claim_teardown(&mut world, Pid(9)));
    }

    #[test]
    fn the_exit_set_leaves_out_the_thread_running_the_exit() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let t1 = world.spawn_thread(pid);
        let t2 = world.spawn_thread(pid);

        let from_main = exit_set(world.get(pid).unwrap(), main);
        assert_eq!(from_main.others, [t1, t2]);
        assert!(from_main.current_is_main);

        let from_sibling = exit_set(world.get(pid).unwrap(), t1);
        assert_eq!(from_sibling.others, [main, t2]);
        assert!(!from_sibling.current_is_main);
    }

    #[test]
    fn a_kill_set_holds_every_thread_including_the_main_one() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let t1 = world.spawn_thread(pid);
        assert_eq!(kill_set(world.get(pid).unwrap()), [main, t1]);
    }

    #[test]
    fn cpu_time_is_charged_to_the_main_thread_only_for_the_main_thread() {
        assert_eq!(charge(Tid(0), Tid(0)), CpuCharge::MainThread);
        assert_eq!(charge(Tid(1), Tid(0)), CpuCharge::ChildThreads);
        // A process whose main thread is not tid 0 — nothing in this crate
        // assumes the loader's numbering.
        assert_eq!(charge(Tid(3), Tid(3)), CpuCharge::MainThread);
        assert_eq!(charge(Tid(0), Tid(3)), CpuCharge::ChildThreads);
    }

    #[test]
    fn a_second_mark_never_moves_a_code_a_thread_chose() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        mark_zombie(&mut world, pid, t1, 7);
        mark_zombie(&mut world, pid, t1, TORN_DOWN_THREAD_CODE);
        assert_eq!(world.get(pid).unwrap().location(t1), Some(ThreadLocation::Zombie(7)));
    }

    #[test]
    fn marking_a_reaped_process_is_silent() {
        let mut world = World::new();
        mark_zombie(&mut world, Pid(4), Tid(0), 0);
    }

    #[test]
    fn the_teardown_sweep_gives_the_main_thread_the_code_and_the_rest_minus_one() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let t1 = world.spawn_thread(pid);
        let t2 = world.spawn_thread(pid);
        mark_zombie(&mut world, pid, t2, 5);

        mark_all_zombie(world.get_mut(pid).unwrap(), 42);
        let proc = world.get(pid).unwrap();
        assert_eq!(proc.location(main), Some(ThreadLocation::Zombie(42)));
        assert_eq!(proc.location(t1), Some(ThreadLocation::Zombie(TORN_DOWN_THREAD_CODE)));
        assert_eq!(
            proc.location(t2),
            Some(ThreadLocation::Zombie(5)),
            "a thread that had already answered for itself kept its own code",
        );
    }

    #[test]
    fn a_main_threads_exit_is_the_processs() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        assert_eq!(route_thread_exit(&world, pid, main), ThreadExit::Process);
    }

    #[test]
    fn an_exit_on_a_reaped_process_leaves_by_the_sibling_door() {
        let world = World::new();
        assert_eq!(route_thread_exit(&world, Pid(3), Tid(1)), ThreadExit::Gone);
    }

    /// Two siblings, and the one that is waiting is not the main thread — the
    /// exact shape the wake-by-name lost, when `thread_exit` posted one wake
    /// and it was always `TaskId(pid, proc.main_tid)`. The release is the exit
    /// pass's `publish_released`, on the dying thread's own watch; nothing else
    /// posts on the exit path.
    #[test]
    fn a_sibling_join_is_released_by_the_sibling_it_named() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let waiter = world.spawn_thread(pid);
        let dying = world.spawn_thread(pid);

        // The joiner arms on the thread it named, which is what
        // `sys_thread_join` does with `Subject::of(sched.handle.watch())`.
        world.arm(Watch::Thread(pid, dying), (pid, waiter));

        let ThreadExit::Sibling = route_thread_exit(&world, pid, dying) else {
            panic!("a non-main thread's exit is a sibling exit");
        };
        world.set_location(pid, dying, ThreadLocation::Zombie(0));
        world.retire(pid, dying);

        assert!(
            world.released(Watch::Thread(pid, dying), (pid, waiter)),
            "the joiner armed on {dying}: the exit pass's release must reach the \
             joiner that named it",
        );
    }
}
