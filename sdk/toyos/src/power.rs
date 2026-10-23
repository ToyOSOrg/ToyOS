//! Asking the supervisor to stop the machine.
//!
//! **The stop is the supervisor's to sequence, because the log is a process.** The
//! kernel stops every userland thread at once and waits for none of them, so
//! whoever asks for the stop has to have made the log whole first — and only
//! the supervisor holds the connection to `/system/bin/logkeeper` that can ask it to. So the supervisor
//! serves [`PORT`]: a holder of its connector asks, the supervisor has `logkeeper` flush,
//! and the supervisor makes the call with the machine's own capability.
//!
//! **A stop asks the login sessions first, and its mode says what then.** The
//! supervisor signs every session out with a terminate. In [`Mode::Wait`] it
//! waits, as events, for them to end, and only a later [`terminate`] from a
//! holder of [`PORT`] forces it; in [`Mode::Terminate`] it kills what stays as
//! soon as the quits are asked. No clock forces either.
//!
//! **No requester waits on the sessions.** [`stop`] is answered at once with
//! [`MSG_PENDING`] and what the stop waits for; later lists come as
//! [`MSG_WAITING`] on the same connection, which a requester may read or
//! drop: dropping it does not cancel the stop. [`cancel`] ends a pending stop
//! and keeps the machine running; a stop that succeeds ends the machine and
//! its requester with it.

use alloc::string::String;
use alloc::vec::Vec;

use toyos_abi::syscall::SyscallError;

use crate::ipc::Connection;

/// The port the supervisor serves the stop on.
pub const PORT: &str = "power";

/// A stop: [`Stop`] and [`Mode`], a byte each.
pub const MSG_STOP: u32 = 1;
/// Force the pending stop: what stays is killed.
pub const MSG_TERMINATE: u32 = 2;
/// End the pending stop; the machine keeps running.
pub const MSG_CANCEL: u32 = 3;
/// The answer to a stop asked or joined: what it waits for, [`Waiting`].
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

/// What a stop does once the sessions are asked to quit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Wait for them to end, however long, until a [`terminate`].
    Wait,
    /// Kill what stays once the quits are asked.
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
        let stop = match self.stop {
            Stop::Reboot => 0,
            Stop::Shutdown => 1,
        };
        let mode = match self.mode {
            Mode::Wait => 0,
            Mode::Terminate => 1,
        };
        [stop, mode]
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let [stop, mode] = bytes else { return None };
        let stop = match stop {
            0 => Stop::Reboot,
            1 => Stop::Shutdown,
            _ => return None,
        };
        let mode = match mode {
            0 => Mode::Wait,
            1 => Mode::Terminate,
            _ => return None,
        };
        Some(Self { stop, mode })
    }
}

/// The most names one list carries, and the longest a name is.
pub const MAX_WAITING: usize = 64;
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

/// Why a request was not done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// This process holds no [`PORT`] connector.
    NotEndowed,
    /// The supervisor could not be asked, or hung up without an answer.
    Unanswered,
    /// The kernel refused the supervisor's call: the machine did not stop.
    Kernel(SyscallError),
    /// A terminate or a cancel, with no stop pending.
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
        let mut out = Vec::new();
        match self {
            Self::NotEndowed | Self::Unanswered => return None,
            Self::Kernel(e) => {
                out.push(0);
                out.extend_from_slice(&e.to_u64().to_le_bytes());
            }
            Self::NoStopPending => out.push(1),
            Self::AnotherStopPending(stop) => {
                out.push(2);
                out.push(Request { stop, mode: Mode::Wait }.encode()[0]);
            }
            Self::TooLate => out.push(3),
        }
        Some(out)
    }

    /// `None` for bytes [`Self::encode`] cannot have written.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0, word @ ..] => SyscallError::from_u64(u64::from_le_bytes(word.try_into().ok()?)).map(Self::Kernel),
            [1] => Some(Self::NoStopPending),
            [2, stop] => Request::decode(&[*stop, 0]).map(|r| Self::AnotherStopPending(r.stop)),
            [3] => Some(Self::TooLate),
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
    /// The supervisor hung up, or said what this protocol has no word for.
    Gone,
}

/// A stop the supervisor took: what it waits for, and its connection, on
/// which [`Self::read`] reads what changes.
pub struct Pending {
    conn: Connection,
    pub waiting: Waiting,
}

impl Pending {
    /// The next word on the stop: blocks until there is one.
    pub fn read(&mut self) -> Update {
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
}

/// Ask the supervisor to stop the machine. Answered at once: the stop taken,
/// with what it waits for, or why it was refused.
pub fn stop(how: Stop, mode: Mode) -> Result<Pending, Refused> {
    let conn = crate::endow::service(PORT).map_err(|_| Refused::NotEndowed)?;
    conn.send_bytes(MSG_STOP, &Request { stop: how, mode }.encode()).map_err(|_| Refused::Unanswered)?;
    match frame(&conn).map_err(|()| Refused::Unanswered)? {
        (MSG_PENDING, payload) => Ok(Pending { waiting: Waiting::decode(&payload).ok_or(Refused::Unanswered)?, conn }),
        (MSG_REFUSED, payload) => Err(Refused::decode(&payload).unwrap_or(Refused::Unanswered)),
        _ => Err(Refused::Unanswered),
    }
}

/// Force the pending stop.
pub fn terminate() -> Result<(), Refused> {
    asked(MSG_TERMINATE)
}

/// End the pending stop, keeping the machine running.
pub fn cancel() -> Result<(), Refused> {
    asked(MSG_CANCEL)
}

fn asked(msg_type: u32) -> Result<(), Refused> {
    let conn = crate::endow::service(PORT).map_err(|_| Refused::NotEndowed)?;
    conn.signal(msg_type).map_err(|_| Refused::Unanswered)?;
    match frame(&conn).map_err(|()| Refused::Unanswered)? {
        (MSG_DONE, _) => Ok(()),
        (MSG_REFUSED, payload) => Err(Refused::decode(&payload).unwrap_or(Refused::Unanswered)),
        _ => Err(Refused::Unanswered),
    }
}

/// One whole frame off `conn`, waiting for it.
fn frame(conn: &Connection) -> Result<(u32, Vec<u8>), ()> {
    let header = conn.recv_header().map_err(drop)?;
    let mut payload = alloc::vec![0u8; header.len() as usize];
    let n = conn.recv_bytes(&header, &mut payload).map_err(drop)?;
    payload.truncate(n);
    Ok((header.msg_type, payload))
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
        for refused in [Refused::Kernel(SyscallError::NotSupported), Refused::NoStopPending, Refused::TooLate] {
            assert_eq!(Refused::decode(&refused.encode().unwrap()), Some(refused));
        }
        for waiting in [Waiting::of([]), Waiting::of(["doom", "editor"])] {
            assert_eq!(Waiting::decode(&waiting.encode()), Some(waiting));
        }
    }

    #[test]
    fn bytes_no_encoder_writes_are_refused() {
        assert_eq!(Request::decode(&[2, 0]), None);
        assert_eq!(Request::decode(&[0, 2]), None);
        assert_eq!(Request::decode(&[0, 0, 0]), None);
        assert_eq!(Refused::decode(&[4]), None);
        assert_eq!(Refused::decode(&[0, 1]), None);
        assert_eq!(Waiting::decode(b"a\n\nb"), None);
        assert!(Refused::NotEndowed.encode().is_none());
    }
}
