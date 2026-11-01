//! Asking the supervisor to stop the machine.
//!
//! **The stop is the supervisor's to sequence, because the log is a process.** The
//! kernel stops every userland thread at once and waits for none of them, so
//! whoever asks for the stop has to have made the log whole first — and only
//! the supervisor holds the connection to `/system/bin/logkeeper` that can ask it to. So the supervisor
//! serves [`PORT`]: a holder of its connector asks, the supervisor has `logkeeper` flush,
//! and the supervisor makes the call with the machine's own capability.
//!
//! **A stop's mode says what it does with the login sessions.** In
//! [`Mode::Wait`] the supervisor signs every session out with a terminate and
//! waits, as events, for them to end; only a later [`terminate`] from a
//! holder of [`PORT`] forces it. In [`Mode::Terminate`] it kills the sessions
//! at once. No clock forces either.
//!
//! **No requester waits on the sessions.** [`stop`] is answered at once with
//! [`MSG_PENDING`], the stop's [`Ticket`] and what it waits for; later lists
//! come as [`MSG_WAITING`] on the same connection, which a requester may read
//! or drop: dropping it does not cancel the stop. [`cancel`] ends a pending
//! stop and keeps the machine running; a stop that succeeds ends the machine
//! and its requester with it.

use alloc::string::String;
use alloc::vec::Vec;

use crate::ipc::Connection;

/// The port the supervisor serves the stop on.
pub const PORT: &str = "power";

/// A stop: [`Stop`] and [`Mode`], a byte each.
pub const MSG_STOP: u32 = 1;
/// Force a pending stop: what stays is killed. Empty, whichever is pending;
/// a [`Ticket`], only that stop.
pub const MSG_TERMINATE: u32 = 2;
/// End the pending stop; the machine keeps running.
pub const MSG_CANCEL: u32 = 3;
/// The answer to a stop asked or joined: [`Answer`].
pub const MSG_PENDING: u32 = 4;
/// What the pending stop waits for now, [`Waiting`].
pub const MSG_WAITING: u32 = 5;
/// The pending stop was cancelled.
pub const MSG_CANCELLED: u32 = 6;
/// A request refused: [`Refused`]'s wire.
pub const MSG_REFUSED: u32 = 7;
/// The answer to a [`MSG_TERMINATE`] or [`MSG_CANCEL`] done.
pub const MSG_DONE: u32 = 8;

/// Which stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Reboot,
    Shutdown,
}

impl Stop {
    fn to_byte(self) -> u8 {
        match self {
            Self::Reboot => 0,
            Self::Shutdown => 1,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Reboot),
            1 => Some(Self::Shutdown),
            _ => None,
        }
    }
}

/// What a stop does with the sessions still open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ask them to quit and wait for them to end, however long, until a
    /// [`terminate`].
    Wait,
    /// Kill them at once.
    Terminate,
}

/// A stop request: [`MSG_STOP`]'s payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub stop: Stop,
    pub mode: Mode,
}

impl Request {
    pub fn encode(self) -> [u8; 2] {
        let mode = match self.mode {
            Mode::Wait => 0,
            Mode::Terminate => 1,
        };
        [self.stop.to_byte(), mode]
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let [stop, mode] = *bytes else { return None };
        let mode = match mode {
            0 => Mode::Wait,
            1 => Mode::Terminate,
            _ => return None,
        };
        Some(Self { stop: Stop::from_byte(stop)?, mode })
    }
}

/// One stop, from its asking to its end: what a terminate names to force that
/// stop and no later one. Never reused in a boot.
pub type Ticket = u64;

/// The most names one list carries.
pub const MAX_WAITING: usize = 64;
/// The longest a name in a list is, in bytes.
pub const MAX_NAME: usize = 32;

/// What a pending stop waits for, by program name, one per line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waiting {
    pub names: Vec<String>,
}

impl Waiting {
    /// `names`, kept to [`MAX_WAITING`] and each to [`MAX_NAME`] bytes.
    pub fn of<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let clip = |name: &'a str| {
            let mut end = name.len().min(MAX_NAME);
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            String::from(&name[..end])
        };
        Self { names: names.into_iter().filter(|n| !n.is_empty()).take(MAX_WAITING).map(clip).collect() }
    }

    pub fn encode(&self) -> Vec<u8> {
        self.names.join("\n").into_bytes()
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(bytes).ok()?;
        let names: Vec<String> = match text.is_empty() {
            true => Vec::new(),
            false => text.split('\n').map(String::from).collect(),
        };
        let valid = |n: &String| !n.is_empty() && n.len() <= MAX_NAME;
        (names.len() <= MAX_WAITING && names.iter().all(valid)).then_some(Self { names })
    }
}

/// [`MSG_PENDING`]'s payload: the stop taken or joined, whether this
/// connection is told what follows, and what it waits for now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub ticket: Ticket,
    /// `false` where the supervisor watches as many requesters as it keeps:
    /// nothing more comes on this connection, and the stop goes on.
    pub watched: bool,
    pub waiting: Waiting,
}

impl Answer {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.ticket.to_le_bytes().to_vec();
        out.push(u8::from(self.watched));
        out.extend(self.waiting.encode());
        out
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (ticket, rest) = bytes.split_first_chunk::<8>()?;
        let (&watched, names) = rest.split_first()?;
        let watched = match watched {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self { ticket: u64::from_le_bytes(*ticket), watched, waiting: Waiting::decode(names)? })
    }
}

/// Why a request was not done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// This process holds no [`PORT`] connector.
    NotEndowed,
    /// The supervisor could not be asked, or hung up without an answer.
    Unanswered,
    /// This machine has no such stop: nothing was ended for it.
    Unsupported,
    /// The machine's stop was asked and did not happen; the sessions it
    /// ended stay ended.
    Failed,
    /// A terminate or a cancel with no stop pending, or a terminate of a stop
    /// that is no longer the one pending.
    NoStopPending,
    /// A stop of another kind is pending, which a cancel ends first.
    AnotherStopPending(Stop),
    /// A cancel once the stop has begun killing.
    TooLate,
}

impl Refused {
    /// [`MSG_REFUSED`]'s payload; `None` for the two this process says
    /// itself.
    pub fn encode(self) -> Option<Vec<u8>> {
        let word = match self {
            Self::NotEndowed | Self::Unanswered => return None,
            Self::Unsupported => alloc::vec![0],
            Self::Failed => alloc::vec![1],
            Self::NoStopPending => alloc::vec![2],
            Self::AnotherStopPending(stop) => alloc::vec![3, stop.to_byte()],
            Self::TooLate => alloc::vec![4],
        };
        Some(word)
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match *bytes {
            [0] => Some(Self::Unsupported),
            [1] => Some(Self::Failed),
            [2] => Some(Self::NoStopPending),
            [3, stop] => Stop::from_byte(stop).map(Self::AnotherStopPending),
            [4] => Some(Self::TooLate),
            _ => None,
        }
    }
}

/// What a pending stop said next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Waiting(Waiting),
    Cancelled,
    /// The machine could not stop.
    Refused(Refused),
    /// The supervisor hung up, said what this protocol has no word for, or
    /// does not watch this connection ([`Answer::watched`]).
    Gone,
}

/// A stop the supervisor took: its ticket, what it waits for, and its
/// connection, on which [`Self::read`] reads what changes.
pub struct Pending {
    conn: Connection,
    pub ticket: Ticket,
    pub watched: bool,
    pub waiting: Waiting,
}

impl Pending {
    /// The next word on the stop: blocks until there is one.
    pub fn read(&mut self) -> Update {
        if !self.watched {
            return Update::Gone;
        }
        let Ok((msg_type, payload)) = frame(&self.conn) else { return Update::Gone };
        match msg_type {
            MSG_WAITING => Waiting::decode(&payload).map_or(Update::Gone, Update::Waiting),
            MSG_CANCELLED => Update::Cancelled,
            MSG_REFUSED => Refused::decode(&payload).map_or(Update::Gone, Update::Refused),
            _ => Update::Gone,
        }
    }

    /// What the stop ended in where the machine did not stop, every list
    /// before it passed over: never [`Update::Waiting`].
    pub fn outcome(mut self) -> Update {
        loop {
            match self.read() {
                Update::Waiting(_) => continue,
                end => return end,
            }
        }
    }

    /// Force this stop, and no other: refused [`Refused::NoStopPending`]
    /// once it has ended, whatever is pending since.
    pub fn terminate(&self) -> Result<(), Refused> {
        asked(MSG_TERMINATE, &self.ticket.to_le_bytes())
    }
}

/// Ask the supervisor to stop the machine. Answered at once: the stop taken,
/// with what it waits for, or why it was refused.
pub fn stop(how: Stop, mode: Mode) -> Result<Pending, Refused> {
    let conn = crate::endow::service(PORT).map_err(|_| Refused::NotEndowed)?;
    conn.send_bytes(MSG_STOP, &Request { stop: how, mode }.encode()).map_err(|_| Refused::Unanswered)?;
    match frame(&conn).map_err(|()| Refused::Unanswered)? {
        (MSG_PENDING, payload) => {
            let Answer { ticket, watched, waiting } = Answer::decode(&payload).ok_or(Refused::Unanswered)?;
            Ok(Pending { conn, ticket, watched, waiting })
        }
        (MSG_REFUSED, payload) => Err(Refused::decode(&payload).unwrap_or(Refused::Unanswered)),
        _ => Err(Refused::Unanswered),
    }
}

/// Force the pending stop, whichever it is.
pub fn terminate() -> Result<(), Refused> {
    asked(MSG_TERMINATE, &[])
}

/// End the pending stop, keeping the machine running.
pub fn cancel() -> Result<(), Refused> {
    asked(MSG_CANCEL, &[])
}

fn asked(msg_type: u32, payload: &[u8]) -> Result<(), Refused> {
    let conn = crate::endow::service(PORT).map_err(|_| Refused::NotEndowed)?;
    conn.send_bytes(msg_type, payload).map_err(|_| Refused::Unanswered)?;
    match frame(&conn).map_err(|()| Refused::Unanswered)? {
        (MSG_DONE, _) => Ok(()),
        (MSG_REFUSED, payload) => Err(Refused::decode(&payload).unwrap_or(Refused::Unanswered)),
        _ => Err(Refused::Unanswered),
    }
}

/// One whole frame off `conn`, waiting for it.
fn frame(conn: &Connection) -> Result<(u32, Vec<u8>), ()> {
    let mut rx = crate::ipc::FrameRx::<{ crate::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(conn).map_err(drop)?;
    Ok((frame.msg_type, rx.payload(frame.payload_len).to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_word_reads_back_as_written() {
        for stop in [Stop::Reboot, Stop::Shutdown] {
            for mode in [Mode::Wait, Mode::Terminate] {
                assert_eq!(Request::decode(&Request { stop, mode }.encode()), Some(Request { stop, mode }));
            }
            let refused = Refused::AnotherStopPending(stop);
            assert_eq!(Refused::decode(&refused.encode().unwrap()), Some(refused));
        }
        for refused in [Refused::Unsupported, Refused::Failed, Refused::NoStopPending, Refused::TooLate] {
            assert_eq!(Refused::decode(&refused.encode().unwrap()), Some(refused));
        }
        for waiting in [Waiting::of([]), Waiting::of(["doom", "editor"])] {
            assert_eq!(Waiting::decode(&waiting.encode()), Some(waiting.clone()));
            for watched in [false, true] {
                let answer = Answer { ticket: u64::MAX - 3, watched, waiting: waiting.clone() };
                assert_eq!(Answer::decode(&answer.encode()), Some(answer));
            }
        }
    }

    #[test]
    fn bytes_no_encoder_writes_are_refused() {
        assert_eq!(Request::decode(&[2, 0]), None);
        assert_eq!(Request::decode(&[0, 2]), None);
        assert_eq!(Request::decode(&[0, 0, 0]), None);
        assert_eq!(Refused::decode(&[5]), None);
        assert_eq!(Refused::decode(&[3, 2]), None);
        assert_eq!(Refused::decode(&[0, 1]), None);
        assert_eq!(Waiting::decode(b"a\n\nb"), None);
        assert_eq!(Answer::decode(&[0; 8]), None);
        assert_eq!(Answer::decode(&[0, 0, 0, 0, 0, 0, 0, 0, 2]), None);
        assert!(Refused::NotEndowed.encode().is_none());
    }
}
