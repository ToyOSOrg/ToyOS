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

use crate::table::{Lifecycle, Processes};
use crate::{Pid, Tid};

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
    match at.zombie_code() {
        Some(code) => {
            proc.forget_thread(tid);
            Ok(Some(code))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;
    use crate::ThreadLocation;

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

    #[test]
    fn the_two_refusals_are_told_apart() {
        let mut world = World::new();
        let pid = world.spawn_process();
        assert_eq!(collect_zombie(&mut world, pid, Tid(9)), Err(JoinRefused::NoSuchThread));
        assert_eq!(collect_zombie(&mut world, Pid(9), Tid(0)), Err(JoinRefused::NoSuchProcess));
    }
}
