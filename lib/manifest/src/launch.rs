//! Who may start what through the supervisor's launcher.
//!
//! **A caller starts only the rows its own row's `starts` lists**: program
//! keys, and [`APPS`] for any installed package. **A row that replaces what the
//! machine runs starts only in a login session** ([`Program::login_only`]). Both
//! are the owner's ruling. A launch is in its caller's session: a row the
//! supervisor starts itself is in the machine's, and a login session is opened
//! for a `login` row ([`Sessions::open`]), never by a launch.
//!
//! **A share is what a file server bounds** ([`Session::share`]): each service
//! the supervisor starts itself has one of its own, each login session has
//! one, and every launch spends its caller's. So no number of launches a
//! service or a login session makes, through whatever its rows' `starts`
//! reach, gives it more of a server.
//!
//! **A login session holds the screen only where the screen's row opened it**
//! ([`Screen`]): the row serving [`consent::PROMPT`], the one whose launches a
//! person at the screen made. Only a launch in a session holding it may ask
//! the person at the screen anything.
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

use crate::{consent, package, Program, MAX_PROGRAM_NAME};

/// The `starts` entry naming every installed package. A program key has no
/// `/`, so it names no key.
pub const APPS: &str = package::DIR;

/// The session a launched program runs in, and the share it spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// A service the supervisor started itself, and every launch made from it
    /// that opens no session.
    Machine(Share),
    /// One the supervisor opened for a `login` row, and every launch made in
    /// it.
    Login(Share, Screen),
}

/// Whether a login session's launches were made by the person at the screen,
/// and may ask them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Held,
    Not,
}

impl Screen {
    /// What a session `opener` opens holds: the screen where `opener` serves
    /// the prompt layer.
    pub fn of(opener: &Program) -> Self {
        match opener.serves.iter().any(|name| name == consent::PROMPT) {
            true => Self::Held,
            false => Self::Not,
        }
    }
}

/// Which share of each file server's bounds: a number only [`Sessions`] makes
/// and a badge it was encoded into gives back, so it is no other service's or
/// session's, nor the supervisor's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Share(u64);

/// The share the supervisor's own files spend, which is no session's.
pub const SUPERVISOR_SHARE: u64 = 0;
const FIRST_SHARE: u64 = 1;

impl Session {
    /// The number a file grant names this session's share of a server by.
    pub fn share(self) -> u64 {
        match self {
            Self::Machine(Share(n)) | Self::Login(Share(n), _) => n,
        }
    }

    /// Whether a launch in this session may ask the person at the screen.
    pub fn at_screen(self) -> bool {
        matches!(self, Self::Login(_, Screen::Held))
    }
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Machine(_) => f.write_str("the machine's session"),
            Self::Login(..) => f.write_str("a login session"),
        }
    }
}

/// The shares the supervisor has handed out.
pub struct Sessions {
    next: u64,
}

impl Default for Sessions {
    fn default() -> Self {
        Self { next: FIRST_SHARE }
    }
}

impl Sessions {
    /// A service the supervisor starts itself: in the machine's session, under
    /// a share of its own that every start of it spends.
    pub fn machine(&mut self) -> Session {
        Session::Machine(self.share())
    }

    /// A login session none before it was.
    pub fn open(&mut self, screen: Screen) -> Session {
        Session::Login(self.share(), screen)
    }

    fn share(&mut self) -> Share {
        let n = self.next;
        self.next = n.checked_add(1).expect("2^64 shares handed out in one boot");
        Share(n)
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
const LOGIN_AT_SCREEN: u8 = 2;

const _: () = assert!(1 + 8 + MAX_PROGRAM_NAME <= MAX_BADGE, "an authority, a kind byte, a share and a row, must fit one badge");

impl Authority {
    /// The session's kind, its share, then the row's key.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 8 + self.row.len());
        out.push(match self.session {
            Session::Machine(_) => MACHINE,
            Session::Login(_, Screen::Not) => LOGIN,
            Session::Login(_, Screen::Held) => LOGIN_AT_SCREEN,
        });
        out.extend_from_slice(&self.session.share().to_le_bytes());
        out.extend_from_slice(self.row.as_bytes());
        out
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&kind, rest) = bytes.split_first()?;
        let (share, row) = rest.split_first_chunk::<8>()?;
        let share = match u64::from_le_bytes(*share) {
            n if n < FIRST_SHARE => return None,
            n => Share(n),
        };
        let session = match kind {
            MACHINE => Session::Machine(share),
            LOGIN => Session::Login(share, Screen::Not),
            LOGIN_AT_SCREEN => Session::Login(share, Screen::Held),
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

/// Whether `caller`, running in `session`, may start `target`.
pub fn may_start(caller: &Program, session: Session, target: Target<'_>) -> Result<(), Refusal> {
    let (listed, program) = match target {
        Target::Row(row) => (row.name.as_str(), row),
        Target::Package(row) => (APPS, row),
    };
    if !caller.starts.iter().any(|key| key == listed) {
        return Err(Refusal::NotListed);
    }
    if program.login_only() && matches!(session, Session::Machine(_)) {
        return Err(Refusal::OutsideLogin);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn row(name: &str) -> Program {
        Program { name: name.into(), source: crate::Source::Path(format!("/system/bin/{name}")), ..Program::default() }
    }

    fn swap() -> Program {
        Program { receives: vec![crate::SWAP_PORT.into()], ..row("swap") }
    }

    fn update() -> Program {
        Program { slots: true, ..row("update") }
    }

    fn caller() -> Program {
        Program { starts: ["ordinary", "swap", "update", "caller", APPS].map(String::from).to_vec(), ..row("caller") }
    }

    /// Every target kind against both kinds of session.
    #[test]
    fn a_caller_starts_what_its_row_lists_and_swap_and_update_only_in_a_login_session() {
        let (ordinary, unlisted, own, package) = (row("ordinary"), row("unlisted"), caller(), row("gbae"));
        let (swap, update) = (swap(), update());
        let mut sessions = Sessions::default();
        let (machine, login) = (sessions.machine(), sessions.open(Screen::Not));
        use Refusal::*;
        #[rustfmt::skip]
        let table: [(Session, Target, Result<(), Refusal>); 12] = [
            (machine, Target::Row(&ordinary), Ok(())),
            (login, Target::Row(&ordinary), Ok(())),
            (machine, Target::Row(&unlisted), Err(NotListed)),
            (login, Target::Row(&unlisted), Err(NotListed)),
            (machine, Target::Row(&own), Ok(())),
            (login, Target::Row(&own), Ok(())),
            (machine, Target::Package(&package), Ok(())),
            (login, Target::Package(&package), Ok(())),
            (machine, Target::Row(&swap), Err(OutsideLogin)),
            (login, Target::Row(&swap), Ok(())),
            (machine, Target::Row(&update), Err(OutsideLogin)),
            (login, Target::Row(&update), Ok(())),
        ];
        for (i, (session, target, want)) in table.into_iter().enumerate() {
            assert_eq!(may_start(&own, session, target), want, "row {i}");
        }
    }

    /// Each service the supervisor starts and each session it opens has a share
    /// of its own, none the supervisor's; only the screen's row's session
    /// holds the screen.
    #[test]
    fn every_service_and_session_has_a_share_of_its_own() {
        let compositor = Program { login: true, serves: vec![consent::PROMPT.into()], ..row("compositor") };
        let sshserver = Program { login: true, ..row("sshserver") };
        let mut sessions = Sessions::default();
        let (a, b) = (sessions.machine(), sessions.machine());
        let desktop = sessions.open(Screen::of(&compositor));
        let remote = sessions.open(Screen::of(&sshserver));
        assert!(desktop.at_screen() && !remote.at_screen());
        assert!(!a.at_screen() && !b.at_screen());
        let shares = [SUPERVISOR_SHARE, a.share(), b.share(), desktop.share(), remote.share()];
        let distinct: BTreeSet<u64> = shares.iter().copied().collect();
        assert_eq!(distinct.len(), shares.len(), "two shares are one: {shares:?}");
    }

    /// A row that lists nothing starts nothing, a package included, and `/apps`
    /// lists no row: a key has no `/`.
    #[test]
    fn nothing_listed_is_nothing_started_and_apps_names_no_row() {
        let none = row("none");
        let (ordinary, package) = (row("ordinary"), row("gbae"));
        let machine = Sessions::default().machine();
        for target in [Target::Row(&ordinary), Target::Package(&package)] {
            assert_eq!(may_start(&none, machine, target), Err(Refusal::NotListed));
        }
        let apps_only = Program { starts: vec![APPS.into()], ..row("apps") };
        assert_eq!(may_start(&apps_only, machine, Target::Row(&ordinary)), Err(Refusal::NotListed));
        assert_eq!(may_start(&apps_only, machine, Target::Package(&package)), Ok(()));
    }

    #[test]
    fn an_authority_reads_back_as_written() {
        let mut sessions = Sessions::default();
        for authority in [
            Authority { row: "terminal".into(), session: sessions.machine() },
            Authority { row: "x".repeat(MAX_PROGRAM_NAME), session: Session::Login(Share(u64::MAX), Screen::Held) },
            Authority { row: "s".into(), session: sessions.open(Screen::Not) },
            Authority { row: "m".into(), session: Session::Machine(Share(u64::MAX)) },
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
        let whole = Authority { row: "shell".into(), session: Sessions::default().open(Screen::Held) };
        let bytes = whole.encode();
        for end in 0..bytes.len() {
            assert_ne!(Authority::decode(&bytes[..end]).as_ref(), Some(&whole), "a prefix of {end} bytes");
        }
        let with = |kind: u8, share: u64, row: &[u8]| [&[kind][..], &share.to_le_bytes(), row].concat();
        assert_eq!(Authority::decode(&[]), None);
        assert_eq!(Authority::decode(&with(3, FIRST_SHARE, b"s")), None);
        for kind in [MACHINE, LOGIN, LOGIN_AT_SCREEN] {
            assert_eq!(Authority::decode(&[kind]), None);
            // Its share cut short.
            assert_eq!(Authority::decode(&[kind, 1, 0, 0, 0, 0, 0, 0]), None);
            // No row.
            assert_eq!(Authority::decode(&with(kind, FIRST_SHARE, b"")), None);
            assert_eq!(Authority::decode(&with(kind, FIRST_SHARE, &[0xff])), None);
            // The supervisor's own share, which no session is.
            assert_eq!(Authority::decode(&with(kind, SUPERVISOR_SHARE, b"s")), None);
            assert_eq!(Authority::decode(&with(kind, FIRST_SHARE, &[b'x'; MAX_PROGRAM_NAME + 1])), None);
        }
    }
}
