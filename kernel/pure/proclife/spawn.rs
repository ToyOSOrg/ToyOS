//! Whether a process may gain another thread, asked twice.
//!
//! **The two questions are one question at two moments, and the second is not
//! redundant.** `process::spawn_thread` reads the parent's TLS template under
//! the table lock, gives the lock up, allocates a combined TLS block, maps it
//! into the address space, rebases the thread pointers inside it, allocates a
//! 128 KiB kernel stack and builds a trampoline frame on it — and only then
//! takes the table lock again to insert the entry and enqueue the task.
//! Everything that makes a thread happens between the two.
//!
//! A teardown claims the process under that same lock
//! ([`crate::proclife::teardown::claim_teardown`]), collects the tids it is going to
//! retire, and gives the lock up to retire them. A thread inserted after that
//! collection is a thread the retire sweep never names.
//! `interleave::tests::a_published_exit_leaves_no_unretired_thread`
//! is that whole sentence as a host test, and
//! `mutate-spawn-skips-the-insert-recheck` is what it reds under.

use crate::proclife::table::{Lifecycle, Processes};
use crate::proclife::Pid;
use toyos_abi::syscall::MAX_THREADS;

/// Whether a new thread may join a process.
#[must_use = "a refused spawn must answer its caller, not fall through"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admit {
    /// Build it.
    Yes,
    /// No such entry: the process was reaped while this spawn was in flight.
    NoSuchProcess,
    /// Somebody owns this process's teardown. A thread admitted now would be
    /// invisible to their retire sweep.
    TearingDown,
    /// The process holds [`MAX_THREADS`] threads. A thread that exits stays
    /// one until it is joined, so only a join makes room.
    Full,
}

impl Admit {
    pub fn is_yes(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// The question at the top of a spawn, before anything is built.
///
/// Asked so the expensive half is not paid for a process that is already going
/// away. It decides nothing on its own: everything it establishes can stop
/// being true before [`admit_thread_insert`] runs.
pub fn admit_thread_start<T: Processes>(table: &T, pid: Pid) -> Admit {
    admit(table.get(pid))
}

/// The same question under the lock that inserts, and **this is the one that
/// decides**.
///
/// It is asked against the same flag `claim_teardown` raises under the same
/// lock, so a thread that passes here is in the table before any retire sweep
/// can begin — which is the ordering `process::spawn_thread` places the
/// scheduler enqueue inside the lock for.
pub fn admit_thread_insert<T: Processes>(table: &T, pid: Pid) -> Admit {
    let proc = table.get(pid);
    // The mutation this feature stages is the *whole* of the second check: an
    // entry that is there is admitted whatever its teardown flag says, which is
    // the answer a kernel that asked the question once would give here.
    #[cfg(feature = "mutate-spawn-skips-the-insert-recheck")]
    if proc.is_some() {
        return Admit::Yes;
    }
    admit(proc)
}

fn admit<P: Lifecycle>(proc: Option<&P>) -> Admit {
    match proc {
        None => Admit::NoSuchProcess,
        Some(proc) if proc.tearing_down() => Admit::TearingDown,
        Some(proc) if proc.thread_count() >= MAX_THREADS => Admit::Full,
        Some(_) => Admit::Yes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proclife::model::World;
    use crate::proclife::{join, teardown, ThreadLocation};

    #[test]
    fn a_live_process_admits_a_thread_at_both_moments() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert_eq!(admit_thread_start(&world, pid), Admit::Yes);
        assert_eq!(admit_thread_insert(&world, pid), Admit::Yes);
    }

    #[test]
    fn a_missing_entry_is_refused_by_name_and_not_by_the_teardown_flag() {
        let world = World::new();
        let gone = Pid(7);
        assert_eq!(admit_thread_start(&world, gone), Admit::NoSuchProcess);
        assert_eq!(admit_thread_insert(&world, gone), Admit::NoSuchProcess);
    }

    /// The shipped decision. Under `mutate-spawn-skips-the-insert-recheck` the
    /// second answer is `Yes`, which is the kernel this tree would have had if
    /// the check were written once — so this case is the mutation's own
    /// fingerprint and is asserted in both arms.
    #[test]
    fn a_claimed_process_refuses_at_the_start_in_both_arms() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(teardown::claim_teardown(&mut world, pid, 137));
        assert_eq!(admit_thread_start(&world, pid), Admit::TearingDown);
    }

    #[cfg(not(feature = "mutate-spawn-skips-the-insert-recheck"))]
    #[test]
    fn a_claimed_process_refuses_at_the_insert_too() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(teardown::claim_teardown(&mut world, pid, 137));
        assert_eq!(admit_thread_insert(&world, pid), Admit::TearingDown);
    }

    /// A zombie holds its place until a join collects it, so the collection is
    /// what admits again.
    #[test]
    fn a_full_process_refuses_at_the_start_until_a_join_collects() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let last = (1..MAX_THREADS).map(|_| world.spawn_thread(pid)).last().unwrap();
        assert_eq!(admit_thread_start(&world, pid), Admit::Full);
        world.set_location(pid, last, ThreadLocation::Zombie(0));
        assert_eq!(admit_thread_start(&world, pid), Admit::Full);
        assert_eq!(join::collect_zombie(&mut world, pid, last), Ok(Some(0)));
        assert_eq!(admit_thread_start(&world, pid), Admit::Yes);
    }

    /// Two spawns pass the start one thread short of the bound; the one whose
    /// sibling inserted first is refused at the insert.
    #[cfg(not(feature = "mutate-spawn-skips-the-insert-recheck"))]
    #[test]
    fn the_insert_refuses_the_spawn_a_sibling_filled_the_process_under() {
        let mut world = World::new();
        let pid = world.spawn_process();
        (2..MAX_THREADS).for_each(|_| _ = world.spawn_thread(pid));
        assert_eq!(admit_thread_start(&world, pid), Admit::Yes);
        world.spawn_thread(pid);
        assert_eq!(admit_thread_insert(&world, pid), Admit::Full);
    }

    #[cfg(feature = "mutate-spawn-skips-the-insert-recheck")]
    #[test]
    fn the_mutation_really_admits_into_a_claimed_teardown() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert!(teardown::claim_teardown(&mut world, pid, 137));
        assert_eq!(
            admit_thread_insert(&world, pid),
            Admit::Yes,
            "the control is inert: the insert still refuses a claimed teardown, \
             so whatever the model reds on under it is not this mutation",
        );
    }
}
