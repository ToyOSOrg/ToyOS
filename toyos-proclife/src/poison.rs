//! What a thread that died in panic recovery leaves to be cleaned up.
//!
//! The panic path itself can do none of it — it may hold any lock the faulted
//! thread was holding — so it records the thread in a per-CPU poison slot and
//! the idle loop runs this later, which is the one context that provably holds
//! none of them.
//!
//! **It is a teardown like any other and takes the same claim.** A poisoned
//! main thread ends its process, so it competes with a `SYS_EXIT` on another
//! thread and with a `SYS_PROCESS_KILL` from a handle holder, and exactly one
//! of the three publishes the exit. What makes this path different is only what
//! it *cannot* do: no resources are released, because every release below it
//! wants a lock the faulted thread may still be recorded as holding, so the
//! process's mappings and handles go with the table entry rather than before
//! it.

use crate::table::{Lifecycle, Processes};
use crate::teardown;
use crate::{Pid, ThreadLocation, Tid, Watch, TORN_DOWN_THREAD_CODE};

/// What must be woken for a poisoned thread, once the table lock is given up.
///
/// Both wakes are carried out by the caller rather than performed here, because
/// both must happen with that lock released.
#[must_use = "a poisoned thread's waiter must be woken"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PoisonOutcome {
    /// Nothing to do: the entry is gone, or another path already owns this
    /// process's teardown and will publish its exit.
    Nothing,
    /// A child thread died. **The subject names the thread that died**, which
    /// is what a `thread_join` arms on — it used to name the process's main
    /// thread, because the wake was by name into a shared parking lot and
    /// whoever was woken re-checked.
    Joiner(Watch),
    /// The main thread died, so the process is over. The exit is published on
    /// the object — outside the table lock, like every other publish — and
    /// whoever holds a handle reads it there.
    Process(Pid),
}

/// Mark a poisoned thread dead and name what must be woken for it.
pub fn zombify_poisoned<T: Processes>(table: &mut T, pid: Pid, tid: Tid) -> PoisonOutcome {
    let Some(proc) = table.get(pid) else { return PoisonOutcome::Nothing };
    let main_tid = proc.main_tid();

    if tid != main_tid {
        if proc.location(tid).is_none() {
            return PoisonOutcome::Nothing;
        }
        let proc = table.get_mut(pid).expect("the entry answered a line ago");
        if proc.location(tid).is_some_and(|l| !l.is_zombie()) {
            proc.set_location(tid, ThreadLocation::Zombie(TORN_DOWN_THREAD_CODE));
        }
        return PoisonOutcome::Joiner(Watch::Thread(pid, tid));
    }

    // The same claim every exit and kill takes, for the same reason: exactly
    // one path publishes one exit.
    if !teardown::claim_teardown(table, pid) {
        return PoisonOutcome::Nothing;
    }
    let proc = table.get_mut(pid).expect("the claim just succeeded on this entry");
    if proc.location(tid).is_none() {
        return PoisonOutcome::Nothing;
    }
    proc.set_location(tid, ThreadLocation::Zombie(TORN_DOWN_THREAD_CODE));
    PoisonOutcome::Process(pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;

    #[test]
    fn a_poisoned_sibling_names_itself_and_not_the_main_thread() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        assert_eq!(zombify_poisoned(&mut world, pid, t1), PoisonOutcome::Joiner(Watch::Thread(pid, t1)));
        assert_eq!(
            world.get(pid).unwrap().location(t1),
            Some(ThreadLocation::Zombie(TORN_DOWN_THREAD_CODE)),
        );
        assert!(
            !world.get(pid).unwrap().tearing_down(),
            "a sibling's death is not the process's, so it takes no claim",
        );
    }

    #[test]
    fn a_poisoned_main_thread_takes_the_claim_and_ends_the_process() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        assert_eq!(zombify_poisoned(&mut world, pid, main), PoisonOutcome::Process(pid));
        assert!(world.get(pid).unwrap().tearing_down());
    }

    /// Not under `mutate-claim-teardown-always-wins`: this asserts the very
    /// exclusion that control removes, so it would red for the mutation rather
    /// than for a law, and the step that reads the arm's verdict lines could
    /// not tell the two apart.
    #[cfg(not(feature = "mutate-claim-teardown-always-wins"))]
    #[test]
    fn a_process_another_path_already_claimed_is_left_alone() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        assert!(teardown::claim_teardown(&mut world, pid));
        assert_eq!(zombify_poisoned(&mut world, pid, main), PoisonOutcome::Nothing);
    }

    #[test]
    fn a_reaped_entry_is_nothing_to_do_rather_than_a_panic() {
        let mut world = World::new();
        assert_eq!(zombify_poisoned(&mut world, Pid(6), Tid(0)), PoisonOutcome::Nothing);
    }

    #[test]
    fn a_sibling_a_join_already_collected_is_nothing_to_do() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        world.forget_thread(pid, t1);
        assert_eq!(zombify_poisoned(&mut world, pid, t1), PoisonOutcome::Nothing);
    }
}
