//! Asking `/system/bin/init` to stop the machine.
//!
//! **The stop is init's to sequence, because the log is a process.** The
//! kernel stops every userland thread at once and waits for none of them, so
//! whoever asks for the stop has to have made the log whole first — and only
//! init holds the connection to `/system/bin/logd` that can ask it to. So init
//! serves [`PORT`]: a holder of its connector asks, init has `logd` flush,
//! and init makes the call with the machine's own capability.
//!
//! The wire is one frame each way: a request, and — only when the machine did
//! not stop — [`MSG_REFUSED`] with the kernel's word.

use toyos_abi::syscall::SyscallError;

use crate::ipc::Connection;

/// The port init serves the stop on.
pub const PORT: &str = "power";

/// Return the machine to firmware.
pub const MSG_REBOOT: u32 = 1;
/// Power the machine off.
pub const MSG_SHUTDOWN: u32 = 2;
/// init could not stop the machine: the payload is the kernel's refusal as a
/// `u64` little-endian syscall word.
pub const MSG_REFUSED: u32 = 3;

/// Which stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Reboot,
    Shutdown,
}

impl Stop {
    pub const fn message(self) -> u32 {
        match self {
            Self::Reboot => MSG_REBOOT,
            Self::Shutdown => MSG_SHUTDOWN,
        }
    }

    /// The request a frame's type names, or `None`.
    pub const fn from_message(msg_type: u32) -> Option<Self> {
        match msg_type {
            MSG_REBOOT => Some(Self::Reboot),
            MSG_SHUTDOWN => Some(Self::Shutdown),
            _ => None,
        }
    }
}

/// Why the machine did not stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// This process holds no [`PORT`] connector.
    NotEndowed,
    /// init could not be asked, or hung up without an answer.
    Unanswered,
    /// The kernel refused init's call.
    Kernel(SyscallError),
}

/// Ask init to stop the machine. Answers only when it did not stop: a stop
/// that happens takes this process with it.
pub fn stop(how: Stop) -> Refused {
    let Ok(conn) = crate::endow::service(PORT) else { return Refused::NotEndowed };
    ask(&conn, how)
}

fn ask(conn: &Connection, how: Stop) -> Refused {
    if conn.signal(how.message()).is_err() {
        return Refused::Unanswered;
    }
    let Ok(header) = conn.recv_header() else { return Refused::Unanswered };
    if header.msg_type != MSG_REFUSED {
        return Refused::Unanswered;
    }
    let mut word = [0u8; 8];
    match conn.recv_bytes(&header, &mut word) {
        Ok(8) => match SyscallError::from_u64(u64::from_le_bytes(word)) {
            Some(e) => Refused::Kernel(e),
            None => Refused::Unanswered,
        },
        _ => Refused::Unanswered,
    }
}
