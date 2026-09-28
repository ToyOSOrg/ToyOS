//! Who ends a process, which threads they retire, and which thread tears it
//! down.
//!
//! **Exactly one path claims a process.** Two of them can arrive at once — a
//! `SYS_EXIT` on one of its threads and a `SYS_PROCESS_KILL` from a holder of a
//! `Process` handle — and [`claim_teardown`] answers `true` to one of them. The
//! claim fixes the exit code, and its winner posts a retire to every thread
//! still in the process ([`retire_set`]) and waits on none of them: a kill's
//! victim may be killing the killer.
//!
//! **The last thread out tears the process down.** Every thread leaves by its
//! own hand ([`leave`]), and the one whose leaving empties a claimed process
//! frees what the process holds and publishes its exit, on its own stack.
//! Nothing a thread can still run in is freed before then, and no thread waits
//! for another to leave. A second publish is an assertion failure in
//! `ProcessObject::publish_exit`, by design.

use alloc::vec::Vec;

use crate::table::{Lifecycle, Processes};
use crate::{Pid, ThreadLocation, Tid, Watch, TORN_DOWN_THREAD_CODE};

/// Claim exclusive teardown of a process, for `code`.
///
/// Exactly one exit or kill path wins; a later caller's thread simply
/// leaves. `false` also covers a process that is not in the table at all,
/// because there is nothing left for a second claimant to do either way.
#[must_use = "a caller that did not win the claim must not retire anything"]
pub fn claim_teardown<T: Processes>(table: &mut T, pid: Pid, code: i32) -> bool {
    let Some(proc) = table.get_mut(pid) else { return false };
    // The mutation this feature stages is the whole of the exclusion: the flag
    // is still raised and still readable, and every arrival is still told it
    // may proceed.
    #[cfg(not(feature = "mutate-claim-teardown-always-wins"))]
    if proc.tearing_down() {
        return false;
    }
    proc.begin_teardown(code);
    true
}

/// The threads a claim's winner retires: every one still in the process but
/// `caller`, which leaves by its own exit. Sorted, so the retire order does not
/// depend on a hash seed.
pub fn retire_set<P: Lifecycle>(proc: &P, caller: Option<Tid>) -> Vec<Tid> {
    let mut tids = Vec::new();
    proc.each_thread(&mut |tid, at| {
        if !at.is_zombie() && Some(tid) != caller {
            tids.push(tid);
        }
    });
    tids.sort_unstable();
    tids
}

/// What a thread's leaving makes it.
#[must_use = "the last thread out owes its process's teardown"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leave {
    /// It emptied a claimed process: it tears the process down and publishes
    /// `code`.
    Last { code: i32 },
    /// Some thread is still in the process, or nobody has claimed it.
    NotLast,
}

/// Take `tid` out of `pid`: dead with the code it chose, else its process's
/// for the main thread and [`TORN_DOWN_THREAD_CODE`] for any other.
///
/// Panics for a thread that is not in its process's entry or has left already:
/// an entry outlives every thread that has not left it.
pub fn leave<T: Processes>(table: &mut T, pid: Pid, tid: Tid, chosen: Option<i32>) -> Leave {
    let proc = table.get_mut(pid).expect("leave: a thread's process is not in the table");
    assert_eq!(
        proc.location(tid),
        Some(ThreadLocation::Scheduled),
        "leave: pid {pid} tid {tid} is not a thread still in its process",
    );
    let claimed = proc.teardown_code();
    let code = match chosen {
        Some(code) => code,
        None if tid == proc.main_tid() => {
            claimed.expect("leave: a main thread leaves only a process somebody claimed")
        }
        None => TORN_DOWN_THREAD_CODE,
    };
    proc.set_location(tid, ThreadLocation::Zombie(code));
    let mut still_in = false;
    proc.each_thread(&mut |_, at| still_in |= !at.is_zombie());
    // The mutation this feature stages: every thread that leaves a claimed
    // process tears it down, the first one out included.
    #[cfg(feature = "mutate-first-out-tears-down")]
    let still_in = false;
    match claimed {
        Some(code) if !still_in => Leave::Last { code },
        _ => Leave::NotLast,
    }
}

/// Which of the two exits a `SYS_THREAD_EXIT` is.
#[must_use = "a thread exit that is not routed is a thread that never dies"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadExit {
    /// The main thread: this is the process's exit.
    Process,
    /// A sibling: release its own mappings, leave, and post on `post` before
    /// the exit pass — after it, this thread does not run again.
    Sibling {
        /// **The subject a joiner armed on, which is the exiting thread's own
        /// watch.** It was the process's main thread until `1bfe4e5b`, because
        /// the wake was by name into a shared parking lot and whoever it
        /// reached re-checked; a non-main thread joining a sibling was owed a
        /// wake nobody sent, and slept until some unrelated wake happened to
        /// reach it.
        post: Watch,
    },
}

/// Route a thread's own exit. Panics for a process not in the table: an entry
/// outlives every thread that has not left it.
pub fn route_thread_exit<T: Processes>(table: &T, pid: Pid, tid: Tid) -> ThreadExit {
    let proc = table.get(pid).expect("route_thread_exit: a thread's process is not in the table");
    if proc.main_tid() == tid {
        return ThreadExit::Process;
    }
    ThreadExit::Sibling { post: Watch::Thread(pid, tid) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;

    #[cfg(not(feature = "mutate-claim-teardown-always-wins"))]
    #[test]
    fn exactly_one_claimant_wins_however_many_arrive() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(claim_teardown(&mut world, pid, 0));
        assert!(!claim_teardown(&mut world, pid, 137));
        assert!(!claim_teardown(&mut world, pid, 137));
        assert_eq!(world.get(pid).unwrap().teardown_code(), Some(0), "the winner's code stands");
    }

    /// Teeth for the control itself: under the mutation a second claimant
    /// really does win, so a red elsewhere is this revert and not something
    /// else.
    #[cfg(feature = "mutate-claim-teardown-always-wins")]
    #[test]
    fn the_mutation_really_grants_a_second_claim() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(claim_teardown(&mut world, pid, 0));
        assert!(
            claim_teardown(&mut world, pid, 137),
            "the control is inert: the claim is still exclusive, so whatever the \
             model reds on under it is not this mutation",
        );
    }

    #[test]
    fn a_process_that_is_gone_grants_no_claim() {
        let mut world = World::new();
        assert!(!claim_teardown(&mut world, Pid(9), 137));
    }

    #[test]
    fn the_retire_set_is_every_thread_still_in_but_the_caller() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let t1 = world.spawn_thread(pid);
        let t2 = world.spawn_thread(pid);
        let t3 = world.spawn_thread(pid);
        world.set_location(pid, t3, ThreadLocation::Zombie(0));

        let proc = world.get(pid).unwrap();
        assert_eq!(retire_set(proc, None), [main, t1, t2]);
        assert_eq!(retire_set(proc, Some(main)), [t1, t2]);
        assert_eq!(retire_set(proc, Some(t1)), [main, t2]);
    }

    #[cfg(not(feature = "mutate-first-out-tears-down"))]
    #[test]
    fn only_the_thread_that_empties_a_claimed_process_tears_it_down() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let t1 = world.spawn_thread(pid);
        let t2 = world.spawn_thread(pid);

        // Unclaimed: a sibling's own exit, with its own code.
        assert_eq!(leave(&mut world, pid, t2, Some(5)), Leave::NotLast);
        assert!(claim_teardown(&mut world, pid, 42));
        assert_eq!(leave(&mut world, pid, main, None), Leave::NotLast);
        assert_eq!(leave(&mut world, pid, t1, None), Leave::Last { code: 42 });

        let proc = world.get(pid).unwrap();
        assert_eq!(proc.location(main), Some(ThreadLocation::Zombie(42)));
        assert_eq!(proc.location(t1), Some(ThreadLocation::Zombie(TORN_DOWN_THREAD_CODE)));
        assert_eq!(proc.location(t2), Some(ThreadLocation::Zombie(5)));
    }

    #[test]
    #[should_panic(expected = "is not a thread still in its process")]
    fn a_thread_leaves_once() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        let _ = leave(&mut world, pid, t1, Some(0));
        let _ = leave(&mut world, pid, t1, Some(0));
    }

    #[test]
    fn a_main_threads_exit_is_the_processs() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        assert_eq!(route_thread_exit(&world, pid, main), ThreadExit::Process);
    }

    /// Two siblings, and the one that is waiting is not the main thread — the
    /// exact shape the wake-by-name lost, when `thread_exit` posted one wake
    /// and it was always `TaskId(pid, proc.main_tid)`.
    #[test]
    fn a_sibling_join_is_released_by_the_sibling_it_named() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let waiter = world.spawn_thread(pid);
        let dying = world.spawn_thread(pid);

        // The joiner arms on the thread it named, which is what
        // `sys_thread_join` does with `Subject::of(sched.handle.watch())`.
        world.arm(Watch::Thread(pid, dying), (pid, waiter));

        let ThreadExit::Sibling { post } = route_thread_exit(&world, pid, dying) else {
            panic!("a non-main thread's exit is a sibling exit");
        };
        world.set_location(pid, dying, ThreadLocation::Zombie(0));
        world.post(post);

        assert!(
            world.released(Watch::Thread(pid, dying), (pid, waiter)),
            "the joiner armed on {dying} and the exit posted on {post:?}: a thread's \
             exit must reach the joiner that named it",
        );
    }
}
