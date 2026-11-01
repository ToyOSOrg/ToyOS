//! What `alarm` answers and arms: the seconds an earlier alarm has left,
//! rounded up and 0 once it is due or when there is none; and 0 seconds arm
//! nothing.

use crate::alarmreq::{due, left, SIG_DFL, SIG_IGN};
use crate::header::{self, SIGNAL_H};

const SEC: u64 = 1_000_000_000;

#[test]
fn an_alarm_is_due_its_seconds_after_it_is_armed_and_zero_arms_none() {
    assert_eq!(due(7, 0), None);
    assert_eq!(due(7, 1), Some(7 + SEC));
    assert_eq!(due(u64::from(u32::MAX), u32::MAX), Some(u64::from(u32::MAX) * (SEC + 1)));
}

#[test]
fn the_seconds_left_round_up_and_end_at_zero() {
    let armed = due(100, 5);
    assert_eq!(left(armed, 100), 5);
    assert_eq!(left(armed, 100 + 1), 5, "a second begun is a second left");
    assert_eq!(left(armed, 100 + SEC), 4);
    assert_eq!(left(armed, 100 + 5 * SEC - 1), 1, "an alarm not yet due answers at least 1");
    assert_eq!(left(armed, 100 + 5 * SEC), 0, "an alarm due answers 0");
    assert_eq!(left(armed, 100 + 6 * SEC), 0, "an alarm past due answers 0");
    assert_eq!(left(None, 100), 0);
    assert_eq!(left(due(0, u32::MAX), 0), u32::MAX);
}

/// Every signal number `sigreq` names is the one `signal.h` gives it, and
/// `SIG_DFL` and `SIG_IGN` its addresses.
#[test]
fn the_signals_libc_names_are_the_numbers_signal_h_gives_them() {
    use crate::sigreq::{SIGALRM, SIGCHLD, SIGHUP, SIGINT, SIGKILL, SIGTERM};
    for (name, number) in [("SIGHUP", SIGHUP), ("SIGINT", SIGINT), ("SIGKILL", SIGKILL), ("SIGALRM", SIGALRM), ("SIGTERM", SIGTERM), ("SIGCHLD", SIGCHLD)] {
        assert_eq!(number, header::int(SIGNAL_H, name), "{name}");
    }
    assert_eq!(SIG_DFL, 0);
    assert_eq!(SIG_IGN, 1);
}
