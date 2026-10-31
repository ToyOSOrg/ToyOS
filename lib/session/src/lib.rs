//! A login session's protocols: the words a login authority, the session's
//! owner, the supervisor and `logout` say to one another, and a login
//! authority's [`open`].
//!
//! **One session owner per login, `/system/bin/session`**, which the supervisor
//! starts and parents and which parents its session's detached apps. It
//! accepts each connection on its session's launcher and hands it to the
//! supervisor over its [`LINK_LABEL`] connection with the caller's badge
//! ([`MSG_HANDOVER`]); the supervisor serves the launch and takes the session
//! from the link, never from the request. It signs the session out
//! ([`SignOut`]) on its [`CONTROL_LABEL`] connections, from the session's login
//! authority, and on the link, from `logout` through [`ADMIN_PORT`].
//!
//! **No clock ends anything.** A graceful sign-out asks every app to quit with
//! a hang-up and reports what stays ([`MSG_WAITING`]) until each has ended;
//! only a forced one kills what remains.

#![forbid(unsafe_code)]

/// The supervisor's port a login authority opens a session on, which only a
/// `login` row receives.
pub const LOGIN_PORT: &str = "login";

/// The supervisor's port `logout` acts on the sessions open through, which
/// only [`ADMIN_HOLDER`] receives, minted with its row and session.
pub const ADMIN_PORT: &str = "session-admin";

/// The one row a build may let receive [`ADMIN_PORT`]: its own binary, so no
/// toybox applet holds it.
pub const ADMIN_HOLDER: &str = "logout";

/// The session owner's row.
pub const OWNER: &str = "session";

/// The label of the namespace the session owner connects its link to the
/// supervisor through, holding [`LINK`]: the owner connects, so the link is
/// counted to the owner's job and not the supervisor's.
pub const LINK_LABEL: &str = "session-link";

/// The name of the link in the owner's [`LINK_LABEL`] namespace.
pub const LINK: &str = "link";

/// The name of a session's controls in a namespace a login authority is
/// handed: it connects its control connection itself, which is then its
/// own, as a launcher's is.
pub const CONTROL: &str = "control";

/// The label of the session launcher's acceptor in the session owner.
pub const LAUNCHER_ACCEPTOR_LABEL: &str = "session-launcher";

/// The label of the acceptor a session's control connections arrive on, in
/// the session owner.
pub const CONTROL_ACCEPTOR_LABEL: &str = "session-controls";

/// The label a login authority the supervisor started with its session holds
/// that session's controls under, a namespace holding [`CONTROL`].
pub const CONTROL_LABEL: &str = "session-control";

/// On [`LOGIN_PORT`]: open a remote session. No payload.
pub const MSG_OPEN: u32 = 1;
/// The answer: the session's launcher, as a namespace holding
/// `toyos::launch::LAUNCHER`, then its controls, a namespace holding
/// [`CONTROL`].
pub const MSG_OPENED: u32 = 2;
/// The answer to a request refused: the reason, UTF-8.
pub const MSG_REFUSED: u32 = 3;

/// On a control connection, and the supervisor's on the link: a [`SignOut`].
pub const MSG_SIGN_OUT: u32 = 10;
/// On a control connection: a graceful sign-out is not to go on.
pub const MSG_CANCEL: u32 = 11;
/// The owner's report, on every control connection and on the link: the
/// apps a sign-out waits for, [`Names`].
pub const MSG_WAITING: u32 = 12;
/// The owner's word that a sign-out was cancelled.
pub const MSG_CANCELLED: u32 = 13;
/// Supervisor to owner: the machine stops, so sign out with a terminate. Owner
/// to controls, a byte: `1`, the sign-out under way is the machine's stop,
/// which `power`'s cancel and terminate decide; `0`, that stop was cancelled
/// and the control's own sign-out goes on.
pub const MSG_STOPPING: u32 = 14;

/// Owner to supervisor: a connection accepted on the session's launcher, its
/// badge the payload and the connection the handle.
pub const MSG_HANDOVER: u32 = 20;
/// Supervisor to owner: an app started in the session, a [`Started`], and a
/// handle to it that may quit and kill it.
pub const MSG_STARTED: u32 = 21;
/// Supervisor to owner: an app of the session ended, its instance as eight
/// little-endian bytes.
pub const MSG_ENDED: u32 = 22;
/// Owner to supervisor, its first word: a handle to itself that places a
/// child under it, which the session's detached apps are spawned under.
pub const MSG_PLACE: u32 = 23;

/// On [`ADMIN_PORT`]: the sessions open. No payload.
pub const MSG_LIST: u32 = 30;
/// The answer to [`MSG_LIST`]: [`Listed`] lines.
pub const MSG_SESSIONS: u32 = 31;
/// On [`ADMIN_PORT`]: a [`SignOutSession`].
pub const MSG_SIGN_OUT_SESSION: u32 = 32;
/// The answer to an admin request done.
pub const MSG_DONE: u32 = 33;

/// Whether a sign-out forces: kills whatever still runs, rather than asking
/// and waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignOut {
    pub force: bool,
}

impl SignOut {
    pub fn encode(self) -> [u8; 1] {
        [self.force as u8]
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0] => Some(Self { force: false }),
            [1] => Some(Self { force: true }),
            _ => None,
        }
    }
}

/// The longest name a [`Names`] or [`Started`] carries: a program key's
/// bound.
pub const MAX_NAME: usize = 32;

/// The most names one [`MSG_WAITING`] carries; a list past it says how many
/// more it leaves out.
pub const MAX_NAMES: usize = 64;

/// Program names, one per line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    pub names: Vec<String>,
    /// How many more there were than [`MAX_NAMES`].
    pub more: u32,
}

impl Names {
    /// `names`, kept to [`MAX_NAMES`] and each to [`MAX_NAME`] bytes.
    pub fn of<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut out = Self::default();
        for name in names {
            match out.names.len() < MAX_NAMES {
                true => out.names.push(clipped(name).to_string()),
                false => out.more += 1,
            }
        }
        out
    }

    /// The count of names left out, then each name after a newline.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.more.to_le_bytes().to_vec();
        for name in &self.names {
            out.push(b'\n');
            out.extend_from_slice(name.as_bytes());
        }
        out
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (more, rest) = bytes.split_first_chunk::<4>()?;
        let text = std::str::from_utf8(rest).ok()?;
        let names: Vec<String> = match text.is_empty() {
            true => Vec::new(),
            false => text.strip_prefix('\n')?.split('\n').map(String::from).collect(),
        };
        let valid = |n: &String| !n.is_empty() && n.len() <= MAX_NAME;
        (names.len() <= MAX_NAMES && names.iter().all(valid)).then(|| Self { names, more: u32::from_le_bytes(*more) })
    }
}

/// `name` to at most [`MAX_NAME`] bytes, on a character boundary.
fn clipped(name: &str) -> &str {
    let mut end = name.len().min(MAX_NAME);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// An app started in a session: the instance the supervisor numbers it by,
/// never reused in a boot, and its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    pub instance: u64,
    pub name: String,
}

impl Started {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.instance.to_le_bytes().to_vec();
        out.extend_from_slice(clipped(&self.name).as_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (instance, name) = bytes.split_first_chunk::<8>()?;
        let name = std::str::from_utf8(name).ok()?;
        (!name.is_empty() && name.len() <= MAX_NAME)
            .then(|| Self { instance: u64::from_le_bytes(*instance), name: name.to_string() })
    }
}

/// An instance, as [`MSG_ENDED`] carries it.
pub fn decode_instance(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// `logout`'s sign-out of one session, by its share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignOutSession {
    pub share: u64,
    pub sign_out: SignOut,
}

impl SignOutSession {
    pub fn encode(self) -> [u8; 9] {
        let mut out = [0u8; 9];
        out[..8].copy_from_slice(&self.share.to_le_bytes());
        out[8] = self.sign_out.force as u8;
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (share, force) = bytes.split_first_chunk::<8>()?;
        Some(Self { share: u64::from_le_bytes(*share), sign_out: SignOut::decode(force)? })
    }
}

/// Whether a session is in use, or signing out and waiting for its apps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Active,
    Ending,
}

/// One session, as [`MSG_SESSIONS`] lists it: its share, whether the screen's
/// row opened it, its state and what its sign-out waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub share: u64,
    pub desktop: bool,
    pub state: State,
    pub waiting: Names,
}

impl Listed {
    /// One line: `<share> desktop|remote active|ending [name ...]`.
    pub fn line(&self) -> String {
        let kind = if self.desktop { "desktop" } else { "remote" };
        let state = match self.state {
            State::Active => "active",
            State::Ending => "ending",
        };
        let mut line = format!("{} {kind} {state}", self.share);
        for name in &self.waiting.names {
            line.push(' ');
            line.push_str(name);
        }
        if self.waiting.more > 0 {
            line.push_str(&format!(" (+{})", self.waiting.more));
        }
        line
    }
}

/// A session opened on [`LOGIN_PORT`] through `login`, the namespace a
/// login authority is endowed under that name: its launcher, as a namespace
/// holding `toyos::launch::LAUNCHER`, and its control connection.
pub fn open(
    login: &toyos::namespace::Namespace,
) -> Result<(toyos::namespace::Namespace, toyos::ipc::Connection), String> {
    let conn = login.open(LOGIN_PORT).map_err(|e| format!("the supervisor's login port refused: {e:?}"))?;
    conn.signal(MSG_OPEN).map_err(|e| format!("the open was not sent: {e:?}"))?;
    let mut rx = Box::new(toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new());
    let frame = rx.recv(&conn).map_err(|e| format!("the open was not answered: {e:?}"))?;
    if frame.msg_type != MSG_OPENED {
        let why = String::from_utf8_lossy(rx.payload(frame.payload_len));
        return Err(format!("the supervisor refused a session: {why}"));
    }
    let [launcher, controls] = rx.take_handles::<2>().ok_or("the session came without its handles")?;
    let control = toyos::namespace::Namespace::received(controls)
        .open(CONTROL)
        .map_err(|e| format!("the session's controls refused its control connection: {e:?}"))?;
    Ok((toyos::namespace::Namespace::received(launcher), control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_word_reads_back_as_written() {
        for force in [false, true] {
            assert_eq!(SignOut::decode(&SignOut { force }.encode()), Some(SignOut { force }));
            let session = SignOutSession { share: 7, sign_out: SignOut { force } };
            assert_eq!(SignOutSession::decode(&session.encode()), Some(session));
        }
        let started = Started { instance: u64::MAX, name: "terminal".into() };
        assert_eq!(Started::decode(&started.encode()), Some(started));
        for names in [Names::of([]), Names::of(["doom", "calc"])] {
            assert_eq!(Names::decode(&names.encode()), Some(names));
        }
        assert_eq!(decode_instance(&9u64.to_le_bytes()), Some(9));
    }

    /// A list past its bound keeps its first names and counts the rest, and a
    /// name past a key's bound is cut, so a report always fits its frame.
    #[test]
    fn a_report_is_bounded() {
        let many: Vec<String> = (0..MAX_NAMES + 3).map(|i| format!("app{i}")).collect();
        let names = Names::of(many.iter().map(String::as_str));
        assert_eq!((names.names.len(), names.more), (MAX_NAMES, 3));
        let long = "x".repeat(MAX_NAME + 5);
        assert_eq!(Names::of([long.as_str()]).names[0].len(), MAX_NAME);
        assert!(Names::of([long.as_str()]).encode().len() <= 4 + MAX_NAMES * (1 + MAX_NAME));
    }

    #[test]
    fn bytes_no_encoder_writes_are_refused() {
        assert_eq!(SignOut::decode(&[2]), None);
        assert_eq!(SignOut::decode(&[]), None);
        assert_eq!(SignOutSession::decode(&[0; 8]), None);
        assert_eq!(Started::decode(&[0; 8]), None);
        assert_eq!(Started::decode(&[0; 7]), None);
        assert_eq!(Names::decode(&[0, 0, 0]), None);
        assert_eq!(Names::decode(b"\0\0\0\0x"), None);
        assert_eq!(Names::decode(b"\0\0\0\0\n"), None);
        assert_eq!(Names::decode(b"\0\0\0\0\na\n"), None);
        assert_eq!(decode_instance(&[0; 7]), None);
    }

    #[test]
    fn a_listed_session_is_one_line() {
        let listed = Listed { share: 3, desktop: false, state: State::Ending, waiting: Names::of(["holder"]) };
        assert_eq!(listed.line(), "3 remote ending holder");
    }
}
