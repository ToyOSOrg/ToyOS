//! What a thread that died in panic recovery leaves to be cleaned up.
//!
//! The panic path itself can do none of it — it may hold any lock the faulted
//! thread was holding — so it records the thread in a per-CPU poison slot and
//! the idle loop runs this later, which is the one context that provably holds
//! none of them.
//!
//! **It is the dead thread's leaving, done for it.** A poisoned main thread
//! ends its process, so it takes the same claim an exit or a kill takes and
//! retires the rest; any poisoned thread is out of its process, and when it is
//! the last one out the exit is published here. What makes this path different
//! is only what it *cannot* do: no resources are released, because every
//! release below it wants a lock the faulted thread may still be recorded as
//! holding, so the process's mappings and handles go with the table entry
//! rather than before it.

use alloc::vec::Vec;

use crate::table::{Lifecycle, Processes};
use crate::teardown::{self, Leave};
use crate::{Pid, ThreadLocation, Tid, Watch, TORN_DOWN_THREAD_CODE};

/// What must be done for a poisoned thread, once the table lock is given up.
#[must_use = "a poisoned thread's waiter must be woken"]
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Poisoned {
    /// The threads its process's claim retires: a main thread's death is its
    /// process's.
    pub retire: Vec<Tid>,
    /// A thread that is not the main one: what its joiner armed on.
    pub joiner: Option<Watch>,
    /// It was the last thread out: the exit to publish, with nothing torn down.
    pub exit: Option<i32>,
}

/// Take a poisoned thread out of its process and say what that leaves to do.
pub fn zombify_poisoned<T: Processes>(table: &mut T, pid: Pid, tid: Tid) -> Poisoned {
    let mut owed = Poisoned::default();
    let Some(proc) = table.get_mut(pid) else { return owed };
    let Some(at) = proc.location(tid) else { return owed };
    let main = tid == proc.main_tid();
    if !main {
        owed.joiner = Some(Watch::Thread(pid, tid));
    }
    // Left already, and died on the way out.
    if at != ThreadLocation::Scheduled {
        return owed;
    }
    if main && teardown::claim_teardown(table, pid, TORN_DOWN_THREAD_CODE) {
        let proc = table.get(pid).expect("the claim just succeeded on this entry");
        owed.retire = teardown::retire_set(proc, Some(tid));
    }
    if let Leave::Last { code } = teardown::leave(table, pid, tid, None) {
        owed.exit = Some(code);
    }
    owed
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
        let owed = zombify_poisoned(&mut world, pid, t1);
        assert_eq!(owed, Poisoned { joiner: Some(Watch::Thread(pid, t1)), ..Poisoned::default() });
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
    fn a_poisoned_main_thread_claims_its_process_and_retires_the_rest() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let t1 = world.spawn_thread(pid);
        let owed = zombify_poisoned(&mut world, pid, main);
        assert_eq!(owed, Poisoned { retire: alloc::vec![t1], ..Poisoned::default() });
        assert!(world.get(pid).unwrap().tearing_down());
    }

    #[test]
    fn a_poisoned_main_thread_alone_publishes_its_exit() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        let owed = zombify_poisoned(&mut world, pid, main);
        assert_eq!(owed, Poisoned { exit: Some(TORN_DOWN_THREAD_CODE), ..Poisoned::default() });
    }

    /// Not under `mutate-claim-teardown-always-wins`: this asserts the very
    /// exclusion that control removes, so it would red for the mutation rather
    /// than for a law, and the step that reads the arm's verdict lines could
    /// not tell the two apart.
    #[cfg(not(feature = "mutate-claim-teardown-always-wins"))]
    #[test]
    fn the_last_poisoned_thread_of_a_claimed_process_publishes_the_claims_code() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let main = world.main_tid(pid);
        assert!(teardown::claim_teardown(&mut world, pid, 137));
        assert_eq!(zombify_poisoned(&mut world, pid, main), Poisoned { exit: Some(137), ..Poisoned::default() });
    }

    #[test]
    fn a_reaped_entry_is_nothing_to_do_rather_than_a_panic() {
        let mut world = World::new();
        assert_eq!(zombify_poisoned(&mut world, Pid(6), Tid(0)), Poisoned::default());
    }

    #[test]
    fn a_sibling_a_join_already_collected_is_nothing_to_do() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        world.forget_thread(pid, t1);
        assert_eq!(zombify_poisoned(&mut world, pid, t1), Poisoned::default());
    }
}
