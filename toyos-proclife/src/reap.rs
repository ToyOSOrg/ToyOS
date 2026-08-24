//! Which entries an idle pass may take.
//!
//! **The whole of what replaced reaping.** Nobody has to be entitled to an
//! entry, there is no orphan to adopt and nothing is kept for anyone to read
//! later: the exit code and the final accounting are on the `ProcessObject`,
//! which outlives the entry for as long as a handle to it does. So the question
//! is one bit per process — has it published its exit — and the answer is a set
//! of pids the caller removes.
//!
//! The bit is not in the table, which is why [`Processes::published_exit`]
//! exists. It is `ProcessObject::finished`, stored with a release by
//! `publish_exit` after the teardown has retired every thread, marked every
//! entry and freed everything the process held; the load that pairs with it is
//! the reason a pass that sees `finished` sees all of that too.
//!
//! [`Processes::published_exit`]: crate::table::Processes::published_exit

use alloc::vec::Vec;

use crate::table::Processes;
use crate::Pid;

/// Every process whose exit is published, in pid order.
///
/// Sorted because the caller drops what it removes, and a drop order that
/// depends on a hash seed is one nothing can reproduce.
pub fn finished_pids<T: Processes>(table: &T) -> Vec<Pid> {
    let mut finished = Vec::new();
    table.each_pid(&mut |pid| {
        if table.published_exit(pid) {
            finished.push(pid);
        }
    });
    finished.sort_unstable();
    finished
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;

    #[test]
    fn nothing_is_taken_from_a_table_of_live_processes() {
        let mut world = World::new();
        world.spawn_process();
        world.spawn_process();
        assert!(finished_pids(&world).is_empty());
    }

    #[test]
    fn only_the_published_are_taken_and_they_come_back_in_pid_order() {
        let mut world = World::new();
        let a = world.spawn_process();
        let b = world.spawn_process();
        let c = world.spawn_process();
        world.publish_exit(c, 0);
        world.publish_exit(a, 0);
        assert_eq!(finished_pids(&world), [a, c]);
        assert!(world.get(b).is_some());
    }
}
