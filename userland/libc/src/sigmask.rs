//! A `sigset_t` as `sigemptyset`, `sigfillset` and `sigaddset` make one, and a
//! thread's signal mask as `pthread_sigmask` and `sigprocmask` change it:
//! signal `n` is bit `n - 1` of a `sigset_t`, Linux's layout. It reads and
//! sets nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

pub(crate) const SIG_BLOCK: i32 = 0;
pub(crate) const SIG_UNBLOCK: i32 = 1;
pub(crate) const SIG_SETMASK: i32 = 2;

const SIGKILL: u32 = 9;
const SIGSTOP: u32 = 19;

/// What no mask holds: POSIX drops `SIGKILL` and `SIGSTOP` from a mask
/// without an error.
const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

/// What `sigfillset` answers: every signal a `sigset_t` has a bit for, since
/// no signal number is this library's own.
pub(crate) const FULL: u64 = u64::MAX;

/// `set` with signal `signo` in it, or `None` for a number no bit stands for,
/// which POSIX's `sigaddset` refuses `EINVAL`.
pub(crate) fn with(set: u64, signo: i32) -> Option<u64> {
    let n = u32::try_from(signo).ok().filter(|n| (1..=u64::BITS).contains(n))?;
    Some(set | 1 << (n - 1))
}

/// `mask` changed by `set` as `how` says, or `None` for a `how` that is none
/// of the three, which POSIX refuses `EINVAL`.
pub(crate) fn changed(mask: u64, how: i32, set: u64) -> Option<u64> {
    let next = match how {
        SIG_BLOCK => mask | set,
        SIG_UNBLOCK => mask & !set,
        SIG_SETMASK => set,
        _ => return None,
    };
    Some(next & !UNBLOCKABLE)
}
