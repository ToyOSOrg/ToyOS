//! What `signal` and `sigaction` decide before a disposition is kept. libc
//! delivers no signal to a handler, so a handler is refused rather than kept
//! and never called; what can be kept is a disposition that is already true,
//! the default or ignoring a signal nothing raises. It reads nothing but what
//! it is handed, so the host tests it (`toyos-libc-copies`).

use crate::alarmreq::{SIG_DFL, SIG_IGN};
use crate::errcode::EINVAL;

/// The signal numbers `signal.h`'s `sigset_t` holds.
pub(crate) const SIGNALS: usize = 64;

const SIGKILL: i32 = 9;
const SIGCHLD: i32 = 17;
const SIGSTOP: i32 = 19;

/// What a signal's disposition may be set to here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Default,
    Ignore,
}

impl Kept {
    /// As `signal.h` spells it.
    pub(crate) const fn handler(self) -> usize {
        match self {
            Self::Default => SIG_DFL,
            Self::Ignore => SIG_IGN,
        }
    }
}

/// The index of signal `signum` in a table of [`SIGNALS`], or `EINVAL` for a
/// number that names none.
pub(crate) fn index(signum: i32) -> Result<usize, i32> {
    usize::try_from(signum).ok().filter(|&n| (1..=SIGNALS).contains(&n)).map(|n| n - 1).ok_or(EINVAL)
}

/// What setting `signum`'s disposition to `handler` keeps, or the `errno` it
/// is refused with:
/// - a number that names no signal, and `SIGKILL` and `SIGSTOP`, whose
///   dispositions POSIX lets nobody change, are `EINVAL`;
/// - a function is `EINVAL`, as no handler is ever called;
/// - ignoring `SIGCHLD` is `EINVAL`, as it would ask that ended children be
///   reaped unwaited, which libc does not do.
pub(crate) fn set(signum: i32, handler: usize) -> Result<Kept, i32> {
    index(signum)?;
    match (signum, handler) {
        (SIGKILL | SIGSTOP, _) => Err(EINVAL),
        (_, SIG_DFL) => Ok(Kept::Default),
        (SIGCHLD, SIG_IGN) => Err(EINVAL),
        (_, SIG_IGN) => Ok(Kept::Ignore),
        _ => Err(EINVAL),
    }
}

/// What `raise` of a signal under disposition `kept` does: no handler is ever
/// installed, so the disposition is all there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Raised {
    /// Ignored, by the program or by the signal's default action.
    Nothing,
    /// The default action ends the process, with exit code 128 plus the
    /// signal's number, as an unhandled quit reason does.
    End(i32),
    /// Refused with this `errno`: a number that names no signal; a default
    /// action that stops the process, which ToyOS has no state for; or a
    /// signal the calling thread blocks, which POSIX holds pending and libc
    /// keeps no pending state for.
    Refused(i32),
}

const SIGCONT: i32 = 18;
const SIGTSTP: i32 = 20;
const SIGTTIN: i32 = 21;
const SIGTTOU: i32 = 22;
const SIGURG: i32 = 23;
const SIGWINCH: i32 = 28;

/// What `raise` of `signum` does under disposition `kept`, with the calling
/// thread's signal `mask`, signal `n` its bit `n - 1`.
pub(crate) fn raised(signum: i32, kept: Kept, mask: u64) -> Raised {
    let n = match index(signum) {
        Ok(n) => n,
        Err(errno) => return Raised::Refused(errno),
    };
    if mask & (1 << n) != 0 && kept == Kept::Default {
        return Raised::Refused(crate::errcode::ENOSYS);
    }
    match (signum, kept) {
        (SIGKILL, _) => Raised::End(128 + SIGKILL),
        (_, Kept::Ignore) => Raised::Nothing,
        (SIGCHLD | SIGCONT | SIGURG | SIGWINCH, Kept::Default) => Raised::Nothing,
        (SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU, Kept::Default) => Raised::Refused(crate::errcode::ENOSYS),
        (_, Kept::Default) => Raised::End(128 + signum),
    }
}
