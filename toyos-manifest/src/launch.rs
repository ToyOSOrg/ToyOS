//! Who may start what through `/system/bin/supervisor`'s launcher.
//!
//! **A caller starts only the rows its own row's `starts` lists**: program
//! keys, and [`APPS`] for any installed package. **A row that replaces what the
//! machine runs starts only in a login session** ([`Program::login_only`]). Both
//! are the owner's ruling. A launch by a row marked `login` opens a login
//! session of its own ([`Sessions`]); every other launch is in its caller's
//! session, and a row the supervisor starts at boot is in the machine's.
//!
//! **A session is what a file server shares by** ([`Session::share`]): every
//! process in one spends one share of each server's bounds, so no number of
//! launches made in a session gives it more of a server.
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
    /// One a `login` row's launch opened, and every launch made in it.
    Login(Login),
}

/// Which login session: a number only [`Sessions::open`] makes and a badge it
/// was encoded into gives back, so it is no other session's and no other share's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Login(u64);

/// The share the supervisor's own files spend, which is no session's.
pub const SUPERVISOR_SHARE: u64 = 0;
const MACHINE_SHARE: u64 = 1;
const FIRST_LOGIN: u64 = 2;

impl Session {
    /// The number a file grant names this session's share of a server by.
    pub fn share(self) -> u64 {
        match self {
            Self::Machine => MACHINE_SHARE,
            Self::Login(Login(n)) => n,
        }
    }
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Machine => f.write_str("the machine's session"),
            Self::Login(_) => f.write_str("a login session"),
        }
    }
}

/// The login sessions the supervisor has opened.
pub struct Sessions {
    next: u64,
}

impl Default for Sessions {
    fn default() -> Self {
        Self { next: FIRST_LOGIN }
    }
}

impl Sessions {
    /// A login session none before it was.
    pub fn open(&mut self) -> Session {
        let n = self.next;
        self.next = n.checked_add(1).expect("2^64 login sessions opened in one boot");
        Session::Login(Login(n))
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

const _: () = assert!(1 + 8 + MAX_PROGRAM_NAME <= MAX_BADGE, "an authority, a kind byte, a login's number and a row, must fit one badge");

impl Authority {
    /// The session's kind, a login session's number, then the row's key.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 8 + self.row.len());
        match self.session {
            Session::Machine => out.push(MACHINE),
            Session::Login(Login(n)) => {
                out.push(LOGIN);
                out.extend_from_slice(&n.to_le_bytes());
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
            LOGIN if rest.len() >= 8 => {
                let (n, row) = rest.split_at(8);
                let n = u64::from_le_bytes(n.try_into().expect("eight bytes"));
                if n < FIRST_LOGIN {
                    return None;
                }
                (Session::Login(Login(n)), row)
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

/// Which session a launch [`may_start`] allowed runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Starts {
    /// Its caller's.
    In(Session),
    /// A login session it opens.
    Opening,
}

impl Starts {
    /// The session, opened from `sessions` when the launch opens one.
    pub fn session(self, sessions: &mut Sessions) -> Session {
        match self {
            Self::In(session) => session,
            Self::Opening => sessions.open(),
        }
    }
}

/// Where `target` starts when `caller`, running in `session`, asks for it, or
/// why it does not start.
pub fn may_start(caller: &Program, session: Session, target: Target<'_>) -> Result<Starts, Refusal> {
    let (listed, program) = match target {
        Target::Row(row) => (row.name.as_str(), row),
        Target::Package(row) => (APPS, row),
    };
    if !caller.starts.iter().any(|key| key == listed) {
        return Err(Refusal::NotListed);
    }
    let starts = match caller.login {
        true => Starts::Opening,
        false => Starts::In(session),
    };
    if program.login_only() && starts == Starts::In(Session::Machine) {
        return Err(Refusal::OutsideLogin);
    }
    Ok(starts)
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
        let (machine, login) = (Session::Machine, Sessions::default().open());
        use Refusal::*;
        use Starts::*;
        #[rustfmt::skip]
        let table: [(bool, Session, Target, Result<Starts, Refusal>); 24] = [
            (false, machine, Target::Row(&ordinary), Ok(In(machine))),
            (false, login, Target::Row(&ordinary), Ok(In(login))),
            (false, machine, Target::Row(&unlisted), Err(NotListed)),
            (false, login, Target::Row(&unlisted), Err(NotListed)),
            (false, machine, Target::Row(&own), Ok(In(machine))),
            (false, login, Target::Row(&own), Ok(In(login))),
            (false, machine, Target::Package(&package), Ok(In(machine))),
            (false, login, Target::Package(&package), Ok(In(login))),
            (false, machine, Target::Row(&swap), Err(OutsideLogin)),
            (false, login, Target::Row(&swap), Ok(In(login))),
            (false, machine, Target::Row(&update), Err(OutsideLogin)),
            (false, login, Target::Row(&update), Ok(In(login))),
            (true, machine, Target::Row(&ordinary), Ok(Opening)),
            (true, login, Target::Row(&ordinary), Ok(Opening)),
            (true, machine, Target::Row(&unlisted), Err(NotListed)),
            (true, login, Target::Row(&unlisted), Err(NotListed)),
            (true, machine, Target::Row(&own), Ok(Opening)),
            (true, login, Target::Row(&own), Ok(Opening)),
            (true, machine, Target::Package(&package), Ok(Opening)),
            (true, login, Target::Package(&package), Ok(Opening)),
            (true, machine, Target::Row(&swap), Ok(Opening)),
            (true, login, Target::Row(&swap), Ok(Opening)),
            (true, machine, Target::Row(&update), Ok(Opening)),
            (true, login, Target::Row(&update), Ok(Opening)),
        ];
        for (i, (opens, session, target, want)) in table.into_iter().enumerate() {
            assert_eq!(may_start(&caller(opens), session, target), want, "row {i}");
        }
    }

    /// The shipping desktop's chain: the compositor, a `login` row, launches
    /// terminals; a terminal launches a shell, and a shell launches shells.
    /// Every launch down one chain spends the share its terminal's launch
    /// opened, and no two of the compositor's launches share one, nor either
    /// with the machine or the supervisor.
    #[test]
    fn a_sessions_launches_spend_its_one_share() {
        let compositor = Program { starts: vec!["terminal".into()], login: true, ..row("compositor") };
        let terminal = Program { starts: vec!["shell".into()], ..row("terminal") };
        let shell = Program { starts: vec!["shell".into()], ..row("shell") };
        let mut sessions = Sessions::default();
        let mut opened = Vec::new();
        for _ in 0..2 {
            let mut launch = |caller: &Program, session, target| {
                may_start(caller, session, Target::Row(target)).expect("listed").session(&mut sessions)
            };
            let first = launch(&compositor, Session::Machine, &terminal);
            let mut session = launch(&terminal, first, &shell);
            for _ in 0..8 {
                session = launch(&shell, session, &shell);
            }
            assert_eq!(session.share(), first.share(), "a shell's launches left its session");
            opened.push(first.share());
        }
        assert_ne!(opened[0], opened[1], "two launches of a login row share one session");
        for share in opened {
            assert!(share != Session::Machine.share() && share != SUPERVISOR_SHARE, "{share}");
        }
        assert_ne!(Session::Machine.share(), SUPERVISOR_SHARE);
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
        assert_eq!(
            may_start(&apps_only, Session::Machine, Target::Package(&package)),
            Ok(Starts::In(Session::Machine))
        );
    }

    #[test]
    fn an_authority_reads_back_as_written() {
        for authority in [
            Authority { row: "terminal".into(), session: Session::Machine },
            Authority { row: "x".repeat(MAX_PROGRAM_NAME), session: Session::Login(Login(u64::MAX)) },
            Authority { row: "s".into(), session: Sessions::default().open() },
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
        let whole = Authority { row: "shell".into(), session: Sessions::default().open() };
        let bytes = whole.encode();
        for end in 0..bytes.len() {
            assert_ne!(Authority::decode(&bytes[..end]).as_ref(), Some(&whole), "a prefix of {end} bytes");
        }
        assert_eq!(Authority::decode(&[]), None);
        assert_eq!(Authority::decode(&[2, b's']), None);
        assert_eq!(Authority::decode(&[MACHINE]), None);
        assert_eq!(Authority::decode(&[LOGIN]), None);
        assert_eq!(Authority::decode(&[MACHINE, 0xff]), None);
        // A login session numbered as no session is.
        for n in [SUPERVISOR_SHARE, MACHINE_SHARE] {
            let mut bytes = vec![LOGIN];
            bytes.extend_from_slice(&n.to_le_bytes());
            bytes.push(b's');
            assert_eq!(Authority::decode(&bytes), None, "{n}");
        }
        // A login session's number cut short.
        assert_eq!(Authority::decode(&[LOGIN, 2, 0, 0, 0, 0, 0, 0]), None);
        let mut long = vec![MACHINE];
        long.extend(std::iter::repeat_n(b'x', MAX_PROGRAM_NAME + 1));
        assert_eq!(Authority::decode(&long), None);
    }
}
