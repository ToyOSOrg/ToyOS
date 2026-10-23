//! Login sessions, as far as the supervisor decides about them: whether a
//! login authority may open one, and which of them a user's `logout` may sign
//! out. Pure.
//!
//! **Only a reopen after a failure is budgeted.** A session owner's failure is
//! an end the supervisor did not ask for and no sign-out preceded; an open by
//! the same login authority after one spends [`Reopens`]'s budget, which is
//! [`crate::restart::Budget`]. An open with no failure before it is never
//! counted.

use std::collections::BTreeMap;
use std::time::Instant;

use toyos_abi::end::End;
use toyos_manifest::launch::Session;

use crate::restart::Budget;

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

/// The budget each login authority's opens after a failure spend.
#[derive(Default, Debug)]
pub struct Reopens {
    owed: BTreeMap<String, (bool, Budget)>,
}

/// An open refused because its authority's budget is spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exhausted;

impl Reopens {
    /// `opener`'s session ended: by a sign-out, or a failure.
    pub fn ended(&mut self, opener: &str, signed_out: bool) {
        self.owed.entry(opener.to_string()).or_default().0 = !signed_out;
    }

    /// Whether `opener` may open a session at `now`, spending its budget where
    /// its last session failed.
    pub fn open(&mut self, opener: &str, now: Instant) -> Result<(), Exhausted> {
        let Some((failed, budget)) = self.owed.get_mut(opener) else { return Ok(()) };
        if !*failed {
            return Ok(());
        }
        match budget.spend(now) {
            true => {
                *failed = false;
                Ok(())
            }
            false => Err(Exhausted),
        }
    }
}

/// Why `logout` may not sign a session out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NoSuchSession,
    NotYours,
}

/// Whether `user` may sign out the session `share` names, among `sessions`,
/// each a share and the user it is of. A session is its own user's alone.
pub fn sign_out_target(sessions: &[(u64, &str)], user: &str, share: u64) -> Result<(), Refused> {
    match sessions.iter().find(|(s, _)| *s == share) {
        None => Err(Refused::NoSuchSession),
        Some((_, of)) if *of != user => Err(Refused::NotYours),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::restart::{RESTARTS, WINDOW};
    use std::time::Duration;

    /// Opens with no failure before them are never counted; reopens after a
    /// failure spend the budget, which refuses the fourth inside its window
    /// and is whole again after it, and a sign-out spends nothing.
    #[test]
    fn only_a_reopen_after_a_failure_spends() {
        let mut reopens = Reopens::default();
        let now = Instant::now();
        for _ in 0..10 * RESTARTS {
            assert_eq!(reopens.open("sshserver", now), Ok(()));
            reopens.ended("sshserver", true);
        }
        reopens.ended("compositor", false);
        for _ in 0..RESTARTS {
            assert_eq!(reopens.open("compositor", now), Ok(()));
            reopens.ended("compositor", false);
        }
        assert_eq!(reopens.open("compositor", now), Err(Exhausted));
        let later = now + WINDOW + Duration::from_millis(1);
        assert_eq!(reopens.open("compositor", later), Ok(()));
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
        for end in [End::Exited(1), End::Killed, End::Fault(Fault::Memory)] {
            assert!(!signed_out(end), "{end:?}");
            reopens.ended("compositor", signed_out(end));
        }
        for _ in 0..RESTARTS {
            assert_eq!(reopens.open("compositor", now), Ok(()));
            reopens.ended("compositor", signed_out(End::Killed));
        }
        assert_eq!(reopens.open("compositor", now), Err(Exhausted));
    }

    #[test]
    fn a_user_signs_out_their_own_sessions_alone() {
        let sessions = [(3, "toy"), (4, "guest")];
        assert_eq!(sign_out_target(&sessions, "toy", 3), Ok(()));
        assert_eq!(sign_out_target(&sessions, "toy", 4), Err(Refused::NotYours));
        assert_eq!(sign_out_target(&sessions, "toy", 5), Err(Refused::NoSuchSession));
    }
}
