//! Collecting a dead thread, which is the one lifecycle read that also
//! destroys what it read.
//!
//! `SYS_THREAD_JOIN` asks for a sibling's exit code, and the answer takes the
//! thread out of the table. **The two halves are one decision** because they
//! have to be one critical section in the caller: a joiner that observed the
//! zombie, gave the table lock up and then removed the entry would be racing a
//! second joiner that did the same, and both would answer with a code only one
//! thread ever produced.
//!
//! A refusal is not an error the caller can wait out. Neither
//! [`JoinRefused`] variant becomes reachable again by parking: a process that
//! is gone stays gone, and a tid this process never had never appears.
//! `sys_thread_join` reads it as the terminal answer for exactly that reason.

use core::cell::Cell;

use crate::proclife::table::{Lifecycle, Processes};
use crate::proclife::{Pid, Tid};

/// Why a join may never be answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinRefused {
    /// The caller's own process is not in the table.
    NoSuchProcess,
    /// This process has no such thread — never had one, or a join already took
    /// it.
    NoSuchThread,
}

/// Collect a zombie sibling and take it out of the table, or say what the state
/// is.
///
/// `Ok(None)` is the one answer that means *wait*: the thread is there and
/// still alive, so the caller arms on it and parks.
#[must_use = "a join's three answers are three different things for the caller to do"]
pub fn collect_zombie<T: Processes>(
    table: &mut T,
    pid: Pid,
    tid: Tid,
) -> Result<Option<i32>, JoinRefused> {
    let proc = table.get_mut(pid).ok_or(JoinRefused::NoSuchProcess)?;
    let at = proc.location(tid).ok_or(JoinRefused::NoSuchThread)?;
    // A thread that left a claimed process still holds mappings its siblings
    // may run in until the last one out; the joiner leaves with it instead.
    #[cfg(not(feature = "mutate-join-collects-in-a-teardown"))]
    if proc.tearing_down() {
        return Ok(None);
    }
    match at.zombie_code() {
        Some(code) => {
            proc.forget_thread(tid);
            Ok(Some(code))
        }
        None => Ok(None),
    }
}

/// One join's answer, kept from the ask that settled it.
///
/// Collecting takes the zombie out of the table, so an ask after the one that
/// collected finds [`JoinRefused::NoSuchThread`]: a join whose wait asks again
/// after every wake answers with its first settled ask, never its last. The
/// answer lives here, behind `&self`, so every ask of one join reads the same
/// answer.
#[derive(Default, Debug)]
pub struct Join(Cell<Option<Result<i32, JoinRefused>>>);

impl Join {
    /// Ask `collect` — [`collect_zombie`] under the table lock — unless an
    /// earlier ask settled; `None` is still waiting. A settled join never calls
    /// `collect`, so it never takes the table lock again.
    #[must_use = "a settled join is the answer the syscall returns"]
    pub fn ask(
        &self,
        collect: impl FnOnce() -> Result<Option<i32>, JoinRefused>,
    ) -> Option<Result<i32, JoinRefused>> {
        if self.0.get().is_none() {
            self.0.set(collect().transpose());
        }
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    use crate::proclife::model::World;
    use crate::proclife::ThreadLocation;

    #[test]
    fn a_live_thread_answers_wait_and_stays_in_the_table() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        assert_eq!(collect_zombie(&mut world, pid, t1), Ok(None));
        assert!(world.get(pid).unwrap().location(t1).is_some());
    }

    #[test]
    fn a_zombie_is_answered_once_and_the_second_join_finds_nothing() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        world.set_location(pid, t1, ThreadLocation::Zombie(11));
        assert_eq!(collect_zombie(&mut world, pid, t1), Ok(Some(11)));
        assert_eq!(collect_zombie(&mut world, pid, t1), Err(JoinRefused::NoSuchThread));
    }

    #[cfg(not(feature = "mutate-join-collects-in-a-teardown"))]
    #[test]
    fn a_process_being_torn_down_gives_up_no_thread() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        assert!(crate::proclife::teardown::claim_teardown(&mut world, pid, 137));
        let _ = crate::proclife::teardown::leave(&mut world, pid, t1, None);
        assert_eq!(collect_zombie(&mut world, pid, t1), Ok(None));
        assert!(world.get(pid).unwrap().location(t1).is_some(), "the entry keeps the thread");
    }

    #[test]
    fn a_join_asked_after_it_collected_keeps_its_answer() {
        let world = RefCell::new(World::new());
        let pid = world.borrow_mut().spawn_process();
        let t1 = world.borrow_mut().spawn_thread(pid);
        let join = Join::default();
        let ask = || join.ask(|| collect_zombie(&mut *world.borrow_mut(), pid, t1));
        let settled = || ask().is_some();
        assert_eq!(ask(), None);
        assert!(!settled());
        world.borrow_mut().set_location(pid, t1, ThreadLocation::Zombie(11));
        assert!(settled(), "the wake that found the zombie did not settle the join");
        assert_eq!(
            ask(),
            Some(Ok(11)),
            "a join that collected its thread answered the ask after it with no such thread",
        );
        let refused = Join::default();
        let ask = || refused.ask(|| collect_zombie(&mut *world.borrow_mut(), pid, Tid(9)));
        assert_eq!(ask(), Some(Err(JoinRefused::NoSuchThread)));
        assert_eq!(ask(), Some(Err(JoinRefused::NoSuchThread)));
    }

    /// The syscall's wait asks after every wake, and `collect` is where the
    /// kernel takes `PROCESS_TABLE`: a settled join answers without it.
    #[test]
    fn a_settled_join_does_not_take_the_table_again() {
        let mut world = World::new();
        let pid = world.spawn_process();
        let t1 = world.spawn_thread(pid);
        world.set_location(pid, t1, ThreadLocation::Zombie(11));
        let join = Join::default();
        assert_eq!(join.ask(|| collect_zombie(&mut world, pid, t1)), Some(Ok(11)));
        assert_eq!(join.ask(|| panic!("a settled join took the table lock to ask again")), Some(Ok(11)));
        let refused = Join::default();
        assert_eq!(refused.ask(|| collect_zombie(&mut world, pid, Tid(9))), Some(Err(JoinRefused::NoSuchThread)));
        assert_eq!(
            refused.ask(|| panic!("a refused join took the table lock to ask again")),
            Some(Err(JoinRefused::NoSuchThread)),
        );
    }

    #[test]
    fn the_two_refusals_are_told_apart() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert_eq!(collect_zombie(&mut world, pid, Tid(9)), Err(JoinRefused::NoSuchThread));
        assert_eq!(collect_zombie(&mut world, Pid(9), Tid(0)), Err(JoinRefused::NoSuchProcess));
    }
}
