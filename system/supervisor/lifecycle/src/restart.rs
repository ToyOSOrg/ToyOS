//! When a process the supervisor started is started again after its end.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use toyos_abi::end::End;
use toyos_abi::syscall::SyscallError;
use toyos_manifest::Program;

/// The most starts a service, or one function's processes, spends inside
/// [`WINDOW`] after its first.
pub const RESTARTS: usize = 3;
pub const WINDOW: Duration = Duration::from_secs(10);

/// The starts after the first that a service or a function has spent.
#[derive(Default, Debug)]
pub struct Budget {
    spent: VecDeque<Instant>,
}

impl Budget {
    /// Spend one start at `now`: `false`, spending nothing, once [`RESTARTS`]
    /// have been spent inside [`WINDOW`].
    pub fn spend(&mut self, now: Instant) -> bool {
        while self.spent.front().is_some_and(|at| now.duration_since(*at) > WINDOW) {
            self.spent.pop_front();
        }
        if self.spent.len() >= RESTARTS {
            return false;
        }
        self.spent.push_back(now);
        true
    }
}

/// A wait without parking on a process of the supervisor's own, as its end
/// once it has ended. `WouldBlock` is the one answer that means it runs:
/// any other refusal of a handle the supervisor holds is its own bug.
pub fn exited(answer: Result<End, SyscallError>) -> Option<End> {
    match answer {
        Ok(exit) => Some(exit),
        Err(SyscallError::WouldBlock) => None,
        Err(e) => panic!("supervisor: its own process handle refused a wait: {e:?}"),
    }
}

/// Whether `row`'s process that ended as `end` is owed another: the row is
/// `restart`, and it ended other than by exit 0, which a program says only
/// when it has nothing to serve.
pub fn owed(row: &Program, end: End) -> bool {
    row.restart && !end.success()
}

/// What a service's end leaves the supervisor to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AfterEnd {
    /// Start it again, on the ports it kept.
    Restart,
    /// Close its ports: no start is owed.
    Close,
    /// Close its ports: a start is owed, and its budget is spent.
    GiveUp,
}

pub fn after_end(row: &Program, end: End, budget: &mut Budget, now: Instant) -> AfterEnd {
    if !owed(row, end) {
        AfterEnd::Close
    } else if budget.spend(now) {
        AfterEnd::Restart
    } else {
        AfterEnd::GiveUp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::end::Fault;

    fn row(restart: bool) -> Program {
        Program { restart, serves: vec!["block".into()], ..Program::default() }
    }

    #[test]
    fn a_restart_row_that_ends_other_than_by_exit_0_is_started_again() {
        let now = Instant::now();
        let ends = [End::Exited(1), End::Exited(-1), End::Exited(101), End::Killed, End::Fault(Fault::Memory)];
        for end in ends {
            assert_eq!(after_end(&row(true), end, &mut Budget::default(), now), AfterEnd::Restart, "{end:?}");
        }
        assert_eq!(after_end(&row(true), End::Exited(0), &mut Budget::default(), now), AfterEnd::Close);
        for end in [End::Exited(0), End::Exited(1), End::Fault(Fault::Handle)] {
            assert_eq!(after_end(&row(false), end, &mut Budget::default(), now), AfterEnd::Close, "{end:?}");
        }
    }

    /// The fourth end inside the window is given up on, and a window later the
    /// budget is whole again.
    #[test]
    fn the_budget_is_restarts_inside_the_window() {
        let (mut budget, now) = (Budget::default(), Instant::now());
        for _ in 0..RESTARTS {
            assert_eq!(after_end(&row(true), End::Exited(1), &mut budget, now), AfterEnd::Restart);
        }
        assert_eq!(after_end(&row(true), End::Exited(1), &mut budget, now + WINDOW), AfterEnd::GiveUp);
        let later = now + WINDOW + Duration::from_millis(1);
        assert_eq!(after_end(&row(true), End::Exited(1), &mut budget, later), AfterEnd::Restart);
    }

    #[test]
    fn a_process_runs_only_while_its_wait_would_block() {
        assert_eq!(exited(Ok(End::Exited(3))), Some(End::Exited(3)));
        assert_eq!(exited(Err(SyscallError::WouldBlock)), None);
    }

    /// Read as "still running", a refused wait would wake the loop on every
    /// pass and act on nothing.
    #[test]
    #[should_panic(expected = "refused a wait")]
    fn any_other_answer_to_a_wait_is_the_supervisors_own_bug() {
        exited(Err(SyscallError::PermissionDenied));
    }

    /// An end owed nothing spends nothing.
    #[test]
    fn an_end_owed_nothing_spends_no_start() {
        let (mut budget, now) = (Budget::default(), Instant::now());
        for _ in 0..2 * RESTARTS {
            assert_eq!(after_end(&row(true), End::Exited(0), &mut budget, now), AfterEnd::Close);
        }
        assert!(budget.spend(now));
    }
}
