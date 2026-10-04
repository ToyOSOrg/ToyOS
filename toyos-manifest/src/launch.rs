//! Who may start what through `/system/bin/supervisor`'s launcher.
//!
//! **A caller starts only the rows its own row's `starts` lists**: program
//! keys, and [`APPS`] for any installed package. **A row that replaces what the
//! machine runs starts only in a login session** ([`Program::login_only`]). Both
//! are the owner's ruling. A launch by a row marked `login` opens a login
//! session; every other launch is in its caller's session, and a row the
//! supervisor starts at boot is in the machine's.
//!
//! **The caller is the badge on its connection, not its word** ([`Authority`]):
//! the supervisor mints it on the launcher it endows a row, the kernel stamps it
//! on every connection made through that launcher, and a holder passes the
//! launcher on only by endowing it.
//!
//! A launch is the one way a child holds more than its parent, and the caller's
//! row listing the target is the approval for that: the child holds its own row.

use std::fmt;

use toyos_abi::syscall::MAX_BADGE;

use crate::{package, Program, MAX_PROGRAM_NAME};

/// The `starts` entry naming every installed package. A program key has no
/// `/`, so it names no key.
pub const APPS: &str = package::DIR;

/// The session a launched program runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// A row the supervisor started at boot, and every launch made in it.
    Machine,
    /// One a `login` row opened, and every launch made in it.
    Login,
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Machine => f.write_str("the machine's session"),
            Self::Login => f.write_str("a login session"),
        }
    }
}

/// What the supervisor mints on the launcher it endows `row`: the row a
/// connection's caller holds, and the session it runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    pub row: String,
    pub session: Session,
}

const MACHINE: u8 = 0;
const LOGIN: u8 = 1;

const _: () = assert!(MAX_PROGRAM_NAME < MAX_BADGE, "an authority, a kind byte and a row, must fit one badge");

impl Authority {
    /// The session's kind, then the row's key.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.row.len());
        out.push(match self.session {
            Session::Machine => MACHINE,
            Session::Login => LOGIN,
        });
        out.extend_from_slice(self.row.as_bytes());
        out
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&kind, rest) = bytes.split_first()?;
        let session = match kind {
            MACHINE => Session::Machine,
            LOGIN => Session::Login,
            _ => return None,
        };
        let row = rest;
        if row.is_empty() || row.len() > MAX_PROGRAM_NAME {
            return None;
        }
        Some(Self { row: std::str::from_utf8(row).ok()?.to_string(), session })
    }
}

/// What a launch names, once resolved.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    Row(&'a Program),
    /// An installed package's synthesized row ([`crate::Manifest::app_row`]).
    Package(&'a Program),
}

/// Why a launch was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotListed,
    OutsideLogin,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotListed => "its caller's row does not list it",
            Self::OutsideLogin => "it starts only in a login session",
        })
    }
}

/// The session `target` starts in when `caller`, running in `session`, asks
/// for it, or why it does not start.
pub fn may_start(caller: &Program, session: Session, target: Target<'_>) -> Result<Session, Refusal> {
    let (listed, program) = match target {
        Target::Row(row) => (row.name.as_str(), row),
        Target::Package(row) => (APPS, row),
    };
    if !caller.starts.iter().any(|key| key == listed) {
        return Err(Refusal::NotListed);
    }
    let session = match caller.login {
        true => Session::Login,
        false => session,
    };
    if program.login_only() && session != Session::Login {
        return Err(Refusal::OutsideLogin);
    }
    Ok(session)
}

/// The supervisor's line for a refused launch, which the metal judge reads whole.
pub fn refused(caller: &str, session: Session, target: &str, why: Refusal) -> String {
    format!("supervisor: launcher: {caller} in {session} may not start {target}: {why}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> Program {
        Program { name: name.into(), path: format!("/system/bin/{name}"), ..Program::default() }
    }

    fn swap() -> Program {
        Program { receives: vec![crate::SWAP_PORT.into()], ..row("swap") }
    }

    fn update() -> Program {
        Program { slots: true, ..row("update") }
    }

    fn caller(login: bool) -> Program {
        Program {
            starts: ["ordinary", "swap", "update", "caller", APPS].map(String::from).to_vec(),
            login,
            ..row("caller")
        }
    }

    /// Every target kind against every session and both kinds of caller.
    #[test]
    fn a_caller_starts_what_its_row_lists_and_swap_and_update_only_in_a_login_session() {
        let (ordinary, unlisted, own, package) = (row("ordinary"), row("unlisted"), caller(false), row("gbae"));
        let (swap, update) = (swap(), update());
        let (machine, login) = (Session::Machine, Session::Login);
        use Refusal::*;
        #[rustfmt::skip]
        let table: [(bool, Session, Target, Result<Session, Refusal>); 24] = [
            (false, machine, Target::Row(&ordinary), Ok(machine)),
            (false, login, Target::Row(&ordinary), Ok(login)),
            (false, machine, Target::Row(&unlisted), Err(NotListed)),
            (false, login, Target::Row(&unlisted), Err(NotListed)),
            (false, machine, Target::Row(&own), Ok(machine)),
            (false, login, Target::Row(&own), Ok(login)),
            (false, machine, Target::Package(&package), Ok(machine)),
            (false, login, Target::Package(&package), Ok(login)),
            (false, machine, Target::Row(&swap), Err(OutsideLogin)),
            (false, login, Target::Row(&swap), Ok(login)),
            (false, machine, Target::Row(&update), Err(OutsideLogin)),
            (false, login, Target::Row(&update), Ok(login)),
            (true, machine, Target::Row(&ordinary), Ok(login)),
            (true, login, Target::Row(&ordinary), Ok(login)),
            (true, machine, Target::Row(&unlisted), Err(NotListed)),
            (true, login, Target::Row(&unlisted), Err(NotListed)),
            (true, machine, Target::Row(&own), Ok(login)),
            (true, login, Target::Row(&own), Ok(login)),
            (true, machine, Target::Package(&package), Ok(login)),
            (true, login, Target::Package(&package), Ok(login)),
            (true, machine, Target::Row(&swap), Ok(login)),
            (true, login, Target::Row(&swap), Ok(login)),
            (true, machine, Target::Row(&update), Ok(login)),
            (true, login, Target::Row(&update), Ok(login)),
        ];
        for (i, (opens, session, target, want)) in table.into_iter().enumerate() {
            assert_eq!(may_start(&caller(opens), session, target), want, "row {i}");
        }
    }

    /// A row that lists nothing starts nothing, a package included, and `/apps`
    /// lists no row: a key has no `/`.
    #[test]
    fn nothing_listed_is_nothing_started_and_apps_names_no_row() {
        let none = row("none");
        let (ordinary, package) = (row("ordinary"), row("gbae"));
        for target in [Target::Row(&ordinary), Target::Package(&package)] {
            assert_eq!(may_start(&none, Session::Machine, target), Err(Refusal::NotListed));
        }
        let apps_only = Program { starts: vec![APPS.into()], ..row("apps") };
        assert_eq!(may_start(&apps_only, Session::Machine, Target::Row(&ordinary)), Err(Refusal::NotListed));
        assert_eq!(may_start(&apps_only, Session::Machine, Target::Package(&package)), Ok(Session::Machine));
    }

    #[test]
    fn an_authority_reads_back_as_written() {
        for authority in [
            Authority { row: "terminal".into(), session: Session::Machine },
            Authority { row: "x".repeat(MAX_PROGRAM_NAME), session: Session::Login },
            Authority { row: "s".into(), session: Session::Login },
        ] {
            let bytes = authority.encode();
            assert!(bytes.len() <= MAX_BADGE);
            assert_eq!(Authority::decode(&bytes), Some(authority));
        }
    }

    /// Bytes `encode` cannot have written are refused, and no prefix of an
    /// authority reads back as it.
    #[test]
    fn what_encode_cannot_have_written_is_refused() {
        let whole = Authority { row: "shell".into(), session: Session::Login };
        let bytes = whole.encode();
        for end in 0..bytes.len() {
            assert_ne!(Authority::decode(&bytes[..end]).as_ref(), Some(&whole), "a prefix of {end} bytes");
        }
        assert_eq!(Authority::decode(&[]), None);
        assert_eq!(Authority::decode(&[2, b's']), None);
        assert_eq!(Authority::decode(&[MACHINE]), None);
        assert_eq!(Authority::decode(&[LOGIN]), None);
        assert_eq!(Authority::decode(&[MACHINE, 0xff]), None);
        let mut long = vec![MACHINE];
        long.extend(std::iter::repeat_n(b'x', MAX_PROGRAM_NAME + 1));
        assert_eq!(Authority::decode(&long), None);
    }
}
