//! A signal mask as `pthread_sigmask` changes it, against POSIX's words for
//! each `how`: `SIG_BLOCK` the union, `SIG_UNBLOCK` the intersection with the
//! complement, `SIG_SETMASK` the set itself; `SIGKILL` and `SIGSTOP` never in
//! it; any other `how` refused. A set as `sigaddset` and `sigfillset` make one,
//! against POSIX's words for each.

use crate::header::{self, SIGNAL_H};
use crate::sigmask;

/// Signal `n`'s bit.
fn bit(n: i32) -> u64 {
    1 << (n - 1)
}

#[test]
fn each_how_changes_the_mask_as_posix_says() {
    let named = |name| header::int(SIGNAL_H, name);
    let (block, unblock, set_mask) = (named("SIG_BLOCK"), named("SIG_UNBLOCK"), named("SIG_SETMASK"));
    let [hup, int, usr1, kill, stop] = ["SIGHUP", "SIGINT", "SIGUSR1", "SIGKILL", "SIGSTOP"].map(|s| bit(named(s)));
    assert_eq!(sigmask::changed(hup, block, usr1), Some(hup | usr1));
    assert_eq!(sigmask::changed(hup | usr1, unblock, usr1 | int), Some(hup));
    assert_eq!(sigmask::changed(hup, set_mask, int), Some(int));
    assert_eq!(sigmask::changed(hup, set_mask, 0), Some(0));
    // Asked for, and dropped without an error.
    assert_eq!(sigmask::changed(0, block, kill | stop | usr1), Some(usr1));
    assert_eq!(sigmask::changed(0, set_mask, u64::MAX), Some(!(kill | stop)));
    for how in [-1, 3, 99] {
        assert_eq!(sigmask::changed(hup, how, usr1), None, "how {how}");
    }
}

#[test]
fn sigaddset_adds_its_signal_alone() {
    for n in 1..=64 {
        assert_eq!(sigmask::with(0, n), Some(bit(n)), "signal {n}");
        assert_eq!(sigmask::with(bit(n), n), Some(bit(n)), "signal {n} twice");
    }
    assert_eq!(sigmask::with(bit(1), 10), Some(bit(1) | bit(10)));
    for n in [0, -1, 65, i32::MIN, i32::MAX] {
        assert_eq!(sigmask::with(bit(1), n), None, "signal {n}");
    }
}

/// POSIX's `sigfillset`: every signal `signal.h` defines is in the full set,
/// and the full set is every signal `sigaddset` takes.
#[test]
fn the_full_set_holds_every_signal() {
    let defined = header::signals();
    assert!(defined.len() > 15, "signal.h defines {} signals", defined.len());
    for (name, n) in defined {
        assert_ne!(sigmask::FULL & bit(n), 0, "{name}");
    }
    assert_eq!((1..=64).fold(0, |set, n| sigmask::with(set, n).unwrap()), sigmask::FULL);
}
