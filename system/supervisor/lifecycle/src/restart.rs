//! When a process the supervisor started is started again after its end.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

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

/// Whether `row`'s process that ended with `exit` is owed another: the row is
/// `restart`, and it ended other than by exit 0, which a program says only
/// when it has nothing to serve.
pub fn owed(row: &Program, exit: i32) -> bool {
    row.restart && exit != 0
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

pub fn after_end(row: &Program, exit: i32, budget: &mut Budget, now: Instant) -> AfterEnd {
    if !owed(row, exit) {
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

    fn row(restart: bool) -> Program {
        Program { restart, serves: vec!["block".into()], ..Program::default() }
    }

    #[test]
    fn a_restart_row_that_ends_other_than_by_exit_0_is_started_again() {
        let now = Instant::now();
        for exit in [1, -1, 101, 139] {
            assert_eq!(after_end(&row(true), exit, &mut Budget::default(), now), AfterEnd::Restart, "exit {exit}");
        }
        assert_eq!(after_end(&row(true), 0, &mut Budget::default(), now), AfterEnd::Close);
        for exit in [0, 1, 139] {
            assert_eq!(after_end(&row(false), exit, &mut Budget::default(), now), AfterEnd::Close, "exit {exit}");
        }
    }

    /// The fourth end inside the window is given up on, and a window later the
    /// budget is whole again.
    #[test]
    fn the_budget_is_restarts_inside_the_window() {
        let (mut budget, now) = (Budget::default(), Instant::now());
        for _ in 0..RESTARTS {
            assert_eq!(after_end(&row(true), 1, &mut budget, now), AfterEnd::Restart);
        }
        assert_eq!(after_end(&row(true), 1, &mut budget, now + WINDOW), AfterEnd::GiveUp);
        let later = now + WINDOW + Duration::from_millis(1);
        assert_eq!(after_end(&row(true), 1, &mut budget, later), AfterEnd::Restart);
    }

    /// An end owed nothing spends nothing.
    #[test]
    fn an_end_owed_nothing_spends_no_start() {
        let (mut budget, now) = (Budget::default(), Instant::now());
        for _ in 0..2 * RESTARTS {
            assert_eq!(after_end(&row(true), 0, &mut budget, now), AfterEnd::Close);
        }
        assert!(budget.spend(now));
    }
}
