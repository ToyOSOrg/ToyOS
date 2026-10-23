//! Login sessions, as far as the supervisor decides about them: whether a
//! login authority may open one, where a detached app in one goes, and what
//! an owner's end means. Pure.
//!
//! **Only failures are budgeted.** A session owner's failure is an end the
//! supervisor did not ask for and no sign-out preceded. A login authority
//! whose sessions failed more than [`RESTARTS`] times inside [`WINDOW`] is
//! refused its next open until the oldest failure leaves the window; every
//! other open, any number of logins and `ssh host cmd` sessions included,
//! spends nothing, and an open refused for another reason spends nothing
//! either.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use toyos_abi::end::End;
use toyos_manifest::launch::Session;
use toyos_manifest::Program;

use crate::restart::{RESTARTS, WINDOW};

/// The failures of each login authority's sessions, inside the window.
#[derive(Default, Debug)]
pub struct Reopens {
    failures: BTreeMap<String, VecDeque<Instant>>,
}

/// An open refused because its authority's sessions failed past the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exhausted;

impl Reopens {
    /// One of `opener`'s sessions ended at `now`: by a sign-out, or a failure.
    pub fn ended(&mut self, opener: &str, signed_out: bool, now: Instant) {
        if !signed_out {
            self.failures.entry(opener.to_string()).or_default().push_back(now);
        }
    }

    /// Whether `opener` may open a session at `now`.
    pub fn may_open(&mut self, opener: &str, now: Instant) -> Result<(), Exhausted> {
        let Some(failures) = self.failures.get_mut(opener) else { return Ok(()) };
        while failures.front().is_some_and(|at| now.duration_since(*at) > WINDOW) {
            failures.pop_front();
        }
        match failures.len() > RESTARTS {
            true => Err(Exhausted),
            false => Ok(()),
        }
    }
}

/// Whether a start of `program` in the machine's session holds the machine's
/// launcher. **Never a `login` row's**: its launches are a session's alone, so
/// one that opened none launches nothing, rather than in the machine's.
pub fn holds_machine_launcher(program: &Program) -> bool {
    !program.login
}

/// One open session, as [`desktop_session`] judges it: whether it is a
/// desktop, the login authority that opened it, and whether its owner still
/// runs, which it may not while its end waits to be read.
#[derive(Clone, Copy, Debug)]
pub struct Open<'a> {
    pub desktop: bool,
    pub opener: &'a str,
    pub running: bool,
}

/// The desktop session `opener` may be handed again, by its index: one it
/// opened whose owner still runs. **Never one whose owner has ended** though
/// its end is not read yet: its launcher's port is about to close.
pub fn desktop_session(open: &[Open<'_>], opener: &str) -> Option<usize> {
    open.iter().position(|s| s.desktop && s.opener == opener && s.running)
}

/// The session a launch an owner handed over runs in: its link's, never what
/// the connection's badge names, which must be the same one.
pub fn handed_over(link: Session, badge: Session) -> Result<Session, Session> {
    match badge == link {
        true => Ok(link),
        false => Err(badge),
    }
}

/// Where an app launched detached is spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
    /// In the machine's session, which no owner holds.
    UnderSupervisor,
    /// In a login session, under its owner: the owner's end takes it down.
    UnderOwner,
    /// In a login session whose owner has given no place yet: refused.
    Unplaced,
}

/// Where a detached app goes, in a login session or the machine's, with its
/// owner's place given or not. **Never the supervisor in a login session**:
/// the session's apps end with its owner.
pub fn detached(in_login: bool, owner_placed: bool) -> Placed {
    match (in_login, owner_placed) {
        (false, _) => Placed::UnderSupervisor,
        (true, true) => Placed::UnderOwner,
        (true, false) => Placed::Unplaced,
    }
}

/// Whether a session whose owner ended as `end` was signed out: its owner
/// exits 0 once it has signed the session out, and any other end is a
/// failure, which ends the session all the same.
pub fn signed_out(end: End) -> bool {
    end == End::Exited(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restart::{RESTARTS, WINDOW};
    use std::time::Duration;

    /// Opens with no failure before them are never counted, however many.
    #[test]
    fn ordinary_opens_spend_nothing() {
        let (mut reopens, now) = (Reopens::default(), Instant::now());
        for _ in 0..10 * RESTARTS {
            assert_eq!(reopens.may_open("sshserver", now), Ok(()));
            reopens.ended("sshserver", true, now);
        }
    }

    /// Several sessions of one authority at once: one failing spends no other
    /// login's open, a sibling's sign-out forgets no failure, and only more
    /// than the budget's failures inside the window refuse, until the oldest
    /// leaves it.
    #[test]
    fn failures_are_budgeted_across_an_authoritys_sessions() {
        let (mut reopens, now) = (Reopens::default(), Instant::now());
        reopens.ended("sshserver", false, now);
        assert_eq!(reopens.may_open("sshserver", now), Ok(()), "one failure refused an ordinary login");
        for _ in 0..RESTARTS {
            reopens.ended("sshserver", true, now);
            reopens.ended("sshserver", false, now);
        }
        assert_eq!(reopens.may_open("sshserver", now), Err(Exhausted), "a sibling's sign-out forgot a failure");
        assert_eq!(reopens.may_open("compositor", now), Ok(()));
        let later = now + WINDOW + Duration::from_millis(1);
        assert_eq!(reopens.may_open("sshserver", later), Ok(()));
    }

    #[test]
    fn a_login_row_holds_no_machine_launcher() {
        assert!(!holds_machine_launcher(&Program { login: true, starts: vec!["shell".into()], ..Program::default() }));
        assert!(holds_machine_launcher(&Program { starts: vec!["toybox".into()], ..Program::default() }));
    }

    /// A desktop whose owner ended, its end not read yet, is not handed
    /// again: the next running one is, or none and one is opened.
    #[test]
    fn a_desktop_whose_owner_ended_is_not_handed_again() {
        let ended = Open { desktop: true, opener: "compositor", running: false };
        let running = Open { desktop: true, opener: "compositor", running: true };
        let remote = Open { desktop: false, opener: "sshserver", running: true };
        assert_eq!(desktop_session(&[ended], "compositor"), None);
        assert_eq!(desktop_session(&[remote, ended, running], "compositor"), Some(2));
        assert_eq!(desktop_session(&[running], "sshserver"), None);
    }

    /// An owner hands over only its own session's launches: a badge naming
    /// another session is refused, whichever it names.
    #[test]
    fn a_handed_over_launch_runs_in_its_links_session() {
        use toyos_manifest::launch::{Screen, Sessions};
        let mut sessions = Sessions::default();
        let (mine, other, machine) = (sessions.open(Screen::Not), sessions.open(Screen::Held), sessions.machine());
        assert_eq!(handed_over(mine, mine), Ok(mine));
        assert_eq!(handed_over(mine, other), Err(other));
        assert_eq!(handed_over(mine, machine), Err(machine));
    }

    /// A login session's detached apps are its owner's, never the
    /// supervisor's, so the owner's end takes them down.
    #[test]
    fn a_detached_app_is_placed_under_its_sessions_owner() {
        assert_eq!(detached(true, true), Placed::UnderOwner);
        assert_eq!(detached(true, false), Placed::Unplaced);
        assert_eq!(detached(false, true), Placed::UnderSupervisor);
        assert_eq!(detached(false, false), Placed::UnderSupervisor);
    }

    /// An owner's end other than its sign-out's is the session's end too,
    /// and a failure its authority's next open spends the budget for.
    #[test]
    fn an_owner_that_ends_unasked_ends_its_session_as_a_failure() {
        use toyos_abi::end::Fault;
        assert!(signed_out(End::Exited(0)));
        let mut reopens = Reopens::default();
        let now = Instant::now();
        for end in [End::Exited(1), End::Killed, End::Fault(Fault::Memory), End::Killed] {
            assert!(!signed_out(end), "{end:?}");
            reopens.ended("compositor", signed_out(end), now);
        }
        assert_eq!(reopens.may_open("compositor", now), Err(Exhausted));
    }

}
