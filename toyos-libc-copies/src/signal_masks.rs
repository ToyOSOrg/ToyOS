//! A signal mask as `pthread_sigmask` changes it, against POSIX's words for
//! each `how`: `SIG_BLOCK` the union, `SIG_UNBLOCK` the intersection with the
//! complement, `SIG_SETMASK` the set itself; `SIGKILL` and `SIGSTOP` never in
//! it; any other `how` refused.

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
