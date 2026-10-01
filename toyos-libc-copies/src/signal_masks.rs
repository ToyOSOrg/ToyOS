//! A signal mask as `pthread_sigmask` changes it, against POSIX's words for
//! each `how`: `SIG_BLOCK` the union, `SIG_UNBLOCK` the intersection with the
//! complement, `SIG_SETMASK` the set itself; `SIGKILL` and `SIGSTOP` never in
//! it; any other `how` refused. A set as `sigaddset` and `sigfillset` make one,
//! against POSIX's words for each.

use crate::sigmask::{self, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK};

/// Signal `n`'s bit.
fn bit(n: u32) -> u64 {
    1 << (n - 1)
}

#[test]
fn each_how_changes_the_mask_as_posix_says() {
    let (hup, int, usr1) = (bit(1), bit(2), bit(10));
    let (kill, stop) = (bit(9), bit(19));
    assert_eq!(sigmask::changed(hup, SIG_BLOCK, usr1), Some(hup | usr1));
    assert_eq!(sigmask::changed(hup | usr1, SIG_UNBLOCK, usr1 | int), Some(hup));
    assert_eq!(sigmask::changed(hup, SIG_SETMASK, int), Some(int));
    assert_eq!(sigmask::changed(hup, SIG_SETMASK, 0), Some(0));
    // Asked for, and dropped without an error.
    assert_eq!(sigmask::changed(0, SIG_BLOCK, kill | stop | usr1), Some(usr1));
    assert_eq!(sigmask::changed(0, SIG_SETMASK, u64::MAX), Some(!(kill | stop)));
    for how in [-1, 3, 99] {
        assert_eq!(sigmask::changed(hup, how, usr1), None, "how {how}");
    }
}

/// POSIX's `sigaddset`: a number no signal has is refused, `EINVAL` (`None`),
/// and any other is added alone, however often.
#[test]
fn sigaddset_adds_its_signal_alone_and_refuses_a_number_no_signal_has() {
    for n in 1..=64 {
        assert_eq!(sigmask::with(0, n), Some(bit(n as u32)), "signal {n}");
        assert_eq!(sigmask::with(bit(n as u32), n), Some(bit(n as u32)), "signal {n} twice");
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
    let defined: Vec<u32> = include_str!("../../userland/libc/include/signal.h")
        .lines()
        .filter_map(|line| line.strip_prefix("#define SIG"))
        .filter(|rest| !rest.starts_with('_'))
        .map(|rest| {
            let value = rest.split_whitespace().nth(1);
            value.and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("signal.h: SIG{rest}"))
        })
        .collect();
    assert!(defined.len() > 15, "signal.h defines {} signals", defined.len());
    for n in defined {
        assert_ne!(sigmask::FULL & bit(n), 0, "signal {n}");
    }
    assert_eq!((1..=64).fold(0, |set, n| sigmask::with(set, n).unwrap()), sigmask::FULL);
}
