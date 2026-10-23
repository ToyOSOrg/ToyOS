//! What a session owner does about its session's apps, pure: `main.rs` holds
//! the handles and acts on what [`Owner`] answers.
//!
//! **A graceful sign-out asks and waits; only a force kills.** It asks every
//! app to quit with a hang-up, which ends at once each one that never listens,
//! and reports the rest until the last has ended, when the owner exits and
//! the session ends. Nothing here reads a clock. A cancel stops the waiting;
//! a hang-up already delivered stays delivered.

use std::collections::BTreeMap;

use toyos_session::Names;

/// What the owner does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Ask this app, and everything below it, to quit with a hang-up.
    Quit(u64),
    Kill(u64),
    /// Tell the session's controls and the supervisor what the sign-out waits for.
    Report(Names),
    Cancelled,
    /// Every app has ended: the session ends with the owner.
    Exit,
}

/// The session's apps, by instance, and whether it is signing out.
#[derive(Debug, Default)]
pub struct Owner {
    apps: BTreeMap<u64, String>,
    signing_out: bool,
}

impl Owner {
    pub fn signing_out(&self) -> bool {
        self.signing_out
    }

    /// An app the supervisor started in the session. One started while the
    /// session signs out is asked to quit as the rest were.
    pub fn started(&mut self, instance: u64, name: String) -> Vec<Act> {
        self.apps.insert(instance, name);
        match self.signing_out {
            true => vec![Act::Quit(instance), self.report()],
            false => Vec::new(),
        }
    }

    pub fn ended(&mut self, instance: u64) -> Vec<Act> {
        self.apps.remove(&instance);
        match self.signing_out {
            true => self.waited(),
            false => Vec::new(),
        }
    }

    pub fn sign_out(&mut self, force: bool) -> Vec<Act> {
        if force {
            let mut acts: Vec<Act> = self.apps.keys().map(|&i| Act::Kill(i)).collect();
            acts.push(Act::Exit);
            return acts;
        }
        let mut acts = match self.signing_out {
            true => Vec::new(),
            false => self.apps.keys().map(|&i| Act::Quit(i)).collect(),
        };
        self.signing_out = true;
        acts.extend(self.waited());
        acts
    }

    pub fn cancel(&mut self) -> Vec<Act> {
        match std::mem::replace(&mut self.signing_out, false) {
            true => vec![Act::Cancelled],
            false => Vec::new(),
        }
    }

    fn waited(&self) -> Vec<Act> {
        match self.apps.is_empty() {
            true => vec![Act::Exit],
            false => vec![self.report()],
        }
    }

    fn report(&self) -> Act {
        Act::Report(Names::of(self.apps.values().map(String::as_str)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(names: &[&str]) -> Act {
        Act::Report(Names::of(names.iter().copied()))
    }

    fn owner(apps: &[(u64, &str)]) -> Owner {
        let mut owner = Owner::default();
        for (instance, name) in apps {
            assert_eq!(owner.started(*instance, name.to_string()), Vec::new());
        }
        owner
    }

    /// A graceful sign-out asks each app once, reports what stays as each
    /// ends, and exits after the last; nothing on that path kills.
    #[test]
    fn a_graceful_sign_out_asks_waits_and_never_kills() {
        let mut owner = owner(&[(1, "terminal"), (2, "holder")]);
        assert_eq!(owner.sign_out(false), vec![Act::Quit(1), Act::Quit(2), waiting(&["terminal", "holder"])]);
        assert_eq!(owner.sign_out(false), vec![waiting(&["terminal", "holder"])], "a second ask asks nobody again");
        assert_eq!(owner.ended(1), vec![waiting(&["holder"])]);
        assert_eq!(owner.started(3, "late".into()), vec![Act::Quit(3), waiting(&["holder", "late"])]);
        assert_eq!(owner.ended(3), vec![waiting(&["holder"])]);
        assert_eq!(owner.ended(2), vec![Act::Exit]);
    }

    /// The holder that never quits stays waited for, whatever else happens,
    /// until a force kills it.
    #[test]
    fn only_a_force_kills() {
        let mut owner = owner(&[(1, "holder"), (2, "doom")]);
        owner.sign_out(false);
        for _ in 0..3 {
            let acts = owner.sign_out(false);
            assert!(!acts.iter().any(|a| matches!(a, Act::Kill(_) | Act::Exit)), "{acts:?}");
        }
        assert_eq!(owner.ended(2), vec![waiting(&["holder"])]);
        assert_eq!(owner.sign_out(true), vec![Act::Kill(1), Act::Exit]);
    }

    /// A cancel stops the waiting, and the session goes on: an app ending
    /// then is no report, and a later sign-out asks again.
    #[test]
    fn a_cancel_keeps_the_session() {
        let mut owner = owner(&[(1, "holder"), (2, "doom")]);
        owner.sign_out(false);
        assert_eq!(owner.cancel(), vec![Act::Cancelled]);
        assert!(!owner.signing_out());
        assert_eq!(owner.ended(2), Vec::new());
        assert_eq!(owner.cancel(), Vec::new(), "nothing to cancel");
        assert_eq!(owner.sign_out(false), vec![Act::Quit(1), waiting(&["holder"])]);
    }

    #[test]
    fn a_session_with_no_app_signs_out_at_once() {
        assert_eq!(Owner::default().sign_out(false), vec![Act::Exit]);
        assert_eq!(Owner::default().sign_out(true), vec![Act::Exit]);
    }
}
