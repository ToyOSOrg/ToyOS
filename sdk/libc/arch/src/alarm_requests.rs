//! What `alarm` answers and arms: the seconds an earlier alarm has left,
//! rounded up and 0 once it is due or when there is none; 0 seconds arm
//! nothing; and a due alarm ends the process unless `SIGALRM` is ignored, the
//! signal `signal.h` numbers.

use crate::alarmreq::{due, ends, left, SIGALRM, SIG_DFL, SIG_IGN};
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

#[test]
fn a_due_alarm_ends_the_process_unless_sigalrm_is_ignored() {
    assert!(!ends(SIG_IGN), "an ignored SIGALRM ended the process");
    assert!(ends(SIG_DFL));
    assert!(ends(0x40_1000), "a handler, which never runs, kept the process");
}

#[test]
fn sigalrm_is_the_number_signal_h_gives_it() {
    assert_eq!(SIGALRM, header::int(SIGNAL_H, "SIGALRM"));
}
