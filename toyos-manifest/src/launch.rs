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
    /// One a `login` row opened, by a never-repeating id.
    Login(u64),
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Machine => f.write_str("the machine's session"),
            Self::Login(id) => write!(f, "login session {id}"),
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

const _: () = assert!(1 + 8 + MAX_PROGRAM_NAME <= MAX_BADGE, "an authority must fit one badge");

impl Authority {
    /// The session's kind, a login session's id, then the row's key.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 8 + self.row.len());
        match self.session {
            Session::Machine => out.push(MACHINE),
            Session::Login(id) => {
                out.push(LOGIN);
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
        out.extend_from_slice(self.row.as_bytes());
        out
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&kind, rest) = bytes.split_first()?;
        let (session, row) = match kind {
            MACHINE => (Session::Machine, rest),
            LOGIN => {
                let (id, row) = rest.split_first_chunk::<8>()?;
                (Session::Login(u64::from_le_bytes(*id)), row)
            }
            _ => return None,
        };
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
/// for it, or why it does not start. `open` names a new login session, and is
/// called only for a listed target of a `login` caller.
pub fn may_start(
    caller: &Program,
    session: Session,
    target: Target<'_>,
    open: impl FnOnce() -> u64,
) -> Result<Session, Refusal> {
    let (listed, program) = match target {
        Target::Row(row) => (row.name.as_str(), row),
        Target::Package(row) => (APPS, row),
    };
    if !caller.starts.iter().any(|key| key == listed) {
        return Err(Refusal::NotListed);
    }
    let session = match caller.login {
        true => Session::Login(open()),
        false => session,
    };
    if program.login_only() && !matches!(session, Session::Login(_)) {
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
        let login = Session::Login(7);
        let opened = Session::Login(9);
        use Refusal::*;
        #[rustfmt::skip]
        let table: [(bool, Session, Target, Result<Session, Refusal>); 24] = [
            (false, Session::Machine, Target::Row(&ordinary), Ok(Session::Machine)),
            (false, login, Target::Row(&ordinary), Ok(login)),
            (false, Session::Machine, Target::Row(&unlisted), Err(NotListed)),
            (false, login, Target::Row(&unlisted), Err(NotListed)),
            (false, Session::Machine, Target::Row(&own), Ok(Session::Machine)),
            (false, login, Target::Row(&own), Ok(login)),
            (false, Session::Machine, Target::Package(&package), Ok(Session::Machine)),
            (false, login, Target::Package(&package), Ok(login)),
            (false, Session::Machine, Target::Row(&swap), Err(OutsideLogin)),
            (false, login, Target::Row(&swap), Ok(login)),
            (false, Session::Machine, Target::Row(&update), Err(OutsideLogin)),
            (false, login, Target::Row(&update), Ok(login)),
            (true, Session::Machine, Target::Row(&ordinary), Ok(opened)),
            (true, login, Target::Row(&ordinary), Ok(opened)),
            (true, Session::Machine, Target::Row(&unlisted), Err(NotListed)),
            (true, login, Target::Row(&unlisted), Err(NotListed)),
            (true, Session::Machine, Target::Row(&own), Ok(opened)),
            (true, login, Target::Row(&own), Ok(opened)),
            (true, Session::Machine, Target::Package(&package), Ok(opened)),
            (true, login, Target::Package(&package), Ok(opened)),
            (true, Session::Machine, Target::Row(&swap), Ok(opened)),
            (true, login, Target::Row(&swap), Ok(opened)),
            (true, Session::Machine, Target::Row(&update), Ok(opened)),
            (true, login, Target::Row(&update), Ok(opened)),
        ];
        for (i, (opens, session, target, want)) in table.into_iter().enumerate() {
            let mut asked = false;
            let got = may_start(&caller(opens), session, target, || {
                asked = true;
                9
            });
            assert_eq!(got, want, "row {i}");
            assert_eq!(asked, opens && got.is_ok(), "row {i}: a login session opened for nothing");
        }
    }

    /// A row that lists nothing starts nothing, a package included, and `/apps`
    /// lists no row: a key has no `/`.
    #[test]
    fn nothing_listed_is_nothing_started_and_apps_names_no_row() {
        let none = row("none");
        let (ordinary, package) = (row("ordinary"), row("gbae"));
        for target in [Target::Row(&ordinary), Target::Package(&package)] {
            assert_eq!(may_start(&none, Session::Machine, target, || 1), Err(Refusal::NotListed));
        }
        let apps_only = Program { starts: vec![APPS.into()], ..row("apps") };
        assert_eq!(may_start(&apps_only, Session::Machine, Target::Row(&ordinary), || 1), Err(Refusal::NotListed));
        assert_eq!(may_start(&apps_only, Session::Machine, Target::Package(&package), || 1), Ok(Session::Machine));
    }

    #[test]
    fn an_authority_reads_back_as_written() {
        for authority in [
            Authority { row: "terminal".into(), session: Session::Machine },
            Authority { row: "x".repeat(MAX_PROGRAM_NAME), session: Session::Login(u64::MAX) },
            Authority { row: "sshserver".into(), session: Session::Login(1) },
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
        let whole = Authority { row: "shell".into(), session: Session::Login(3) };
        let bytes = whole.encode();
        for end in 0..bytes.len() {
            assert_ne!(Authority::decode(&bytes[..end]).as_ref(), Some(&whole), "a prefix of {end} bytes");
        }
        assert_eq!(Authority::decode(&[]), None);
        assert_eq!(Authority::decode(&[LOGIN, 1, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(Authority::decode(&[2, b's']), None);
        assert_eq!(Authority::decode(&[MACHINE]), None);
        assert_eq!(Authority::decode(&[LOGIN, 1, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(Authority::decode(&[MACHINE, 0xff]), None);
        let mut long = vec![MACHINE];
        long.extend(std::iter::repeat_n(b'x', MAX_PROGRAM_NAME + 1));
        assert_eq!(Authority::decode(&long), None);
    }
}
