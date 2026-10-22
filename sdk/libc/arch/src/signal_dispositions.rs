//! What `signal` and `sigaction` keep and refuse (`sigreq.rs`): the default
//! and ignoring are kept, a handler is refused since none is ever called, and
//! so are `SIGKILL`, `SIGSTOP`, an ignored `SIGCHLD` and a number that names no
//! signal, each by the number `signal.h` gives it.

use crate::alarmreq::{SIG_DFL, SIG_IGN};
use crate::errcode::EINVAL;
use crate::header::{self, SIGNAL_H};
use crate::sigreq::{index, set, Kept, SIGNALS};

fn sig(name: &str) -> i32 {
    header::int(SIGNAL_H, name)
}

/// A handler's address, as a program's function would be.
const HANDLER: usize = 0x4000;

#[test]
fn the_default_and_ignoring_are_kept_and_a_handler_is_refused() {
    for name in ["SIGHUP", "SIGINT", "SIGPIPE", "SIGTERM", "SIGUSR1"] {
        assert_eq!(set(sig(name), SIG_DFL), Ok(Kept::Default), "{name}");
        assert_eq!(set(sig(name), SIG_IGN), Ok(Kept::Ignore), "{name}");
        assert_eq!(set(sig(name), HANDLER), Err(EINVAL), "{name}: a handler that is never called");
    }
    assert_eq!(Kept::Default.handler(), SIG_DFL);
    assert_eq!(Kept::Ignore.handler(), SIG_IGN);
}

#[test]
fn kill_stop_an_ignored_child_and_no_signal_are_refused() {
    for handler in [SIG_DFL, SIG_IGN, HANDLER] {
        assert_eq!(set(sig("SIGKILL"), handler), Err(EINVAL));
        assert_eq!(set(sig("SIGSTOP"), handler), Err(EINVAL));
    }
    assert_eq!(set(sig("SIGCHLD"), SIG_IGN), Err(EINVAL), "would reap children unwaited");
    assert_eq!(set(sig("SIGCHLD"), SIG_DFL), Ok(Kept::Default));
    for signum in [0, -1, SIGNALS as i32 + 1] {
        assert_eq!(index(signum), Err(EINVAL), "{signum}");
        assert_eq!(set(signum, SIG_DFL), Err(EINVAL), "{signum}");
    }
    assert_eq!(index(1), Ok(0));
    assert_eq!(index(SIGNALS as i32), Ok(SIGNALS - 1));
}

/// `raise` does what the disposition says, as no handler runs: an ignored
/// signal and one whose default ignores do nothing, a terminating default
/// ends the process with 128 plus the number, and a stop or no signal is
/// refused.
#[test]
fn raise_does_what_the_disposition_says() {
    use crate::errcode::ENOSYS;
    use crate::sigreq::{raised, Raised};
    assert_eq!(raised(sig("SIGTERM"), Kept::Default, 0), Raised::End(128 + sig("SIGTERM")));
    assert_eq!(raised(sig("SIGTERM"), Kept::Ignore, 0), Raised::Nothing);
    assert_eq!(raised(sig("SIGABRT"), Kept::Default, 0), Raised::End(134), "abort's own code");
    assert_eq!(raised(sig("SIGCHLD"), Kept::Default, 0), Raised::Nothing, "a default that ignores");
    assert_eq!(raised(sig("SIGKILL"), Kept::Default, 0), Raised::End(128 + 9));
    assert_eq!(raised(sig("SIGSTOP"), Kept::Default, 0), Raised::Refused(ENOSYS));
    assert_eq!(raised(0, Kept::Default, 0), Raised::Refused(EINVAL));
}

/// A signal the calling thread blocks would be held pending, and libc keeps
/// no pending state, so its raise is refused rather than ending the process;
/// an ignored one is dropped whether blocked or not.
#[test]
fn raise_of_a_blocked_signal_is_refused() {
    use crate::errcode::ENOSYS;
    use crate::sigreq::{raised, Raised};
    let term = sig("SIGTERM");
    let blocked = 1u64 << (term - 1);
    assert_eq!(raised(term, Kept::Default, blocked), Raised::Refused(ENOSYS));
    assert_eq!(raised(term, Kept::Ignore, blocked), Raised::Nothing);
    assert_eq!(raised(term, Kept::Default, !blocked), Raised::End(128 + term), "another signal's bit");
}
