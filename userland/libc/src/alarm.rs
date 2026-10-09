//! `alarm`: one per process, kept by a thread of this library's, started by
//! the first alarm armed, that sleeps until it is due. Then, with `SIGALRM`
//! ignored, the alarm is gone; otherwise the keeper does what `SIGALRM`'s
//! default action does and ends the process. A handler `signal` or `sigaction`
//! is given never runs, and a mask that blocks `SIGALRM` holds nothing back
//! (`issues/an-alarm-reaches-no-sigalrm-handler.md`).

use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::syscall;

use crate::alarmreq;
use crate::misc::SigAction;
use crate::pthread::Lock;

/// What the process ends with when its alarm is due: the code of an end by
/// `SIGALRM`, as `abort`'s is `SIGABRT`'s.
const ALARM_END: i32 = 128 + alarmreq::SIGALRM;

struct Alarm {
    /// When the alarm is due, in the monotonic clock's nanoseconds.
    due: Option<u64>,
    /// Whether the keeper runs.
    kept: bool,
}

static ALARM: Lock<Alarm> = Lock::new(Alarm { due: None, kept: false });

/// Moved at every `alarm`, so a keeper asleep on an older one wakes.
static TURN: AtomicU32 = AtomicU32::new(0);

/// `SIGALRM`'s action, whose handler is its disposition: `SIG_DFL`, `SIG_IGN`
/// or a handler's address.
static ACTION: Lock<SigAction> = Lock::new(SigAction::of(alarmreq::SIG_DFL));

/// Make `new`, if any, `SIGALRM`'s action, and answer the one it replaces whole.
pub(crate) fn act(new: Option<SigAction>) -> SigAction {
    let mut action = ACTION.lock();
    let old = *action;
    if let Some(new) = new {
        *action = new;
    }
    old
}

fn now() -> u64 {
    toyos_abi::clock::nanos_since_boot()
}

#[no_mangle]
pub extern "C" fn alarm(seconds: u32) -> u32 {
    let now = now();
    let mut alarm = ALARM.lock();
    let left = alarmreq::left(alarm.due, now);
    alarm.due = alarmreq::due(now, seconds);
    if alarm.due.is_some() && !alarm.kept {
        // Never joined: it lives as long as the process.
        let mut thread = 0u64;
        // SAFETY: `keep` is a thread entry that takes and returns nothing.
        let refused = unsafe { crate::pthread::pthread_create(&mut thread, ptr::null(), keep, ptr::null_mut()) };
        assert_eq!(refused, 0, "alarm: no thread to keep it");
        alarm.kept = true;
    }
    TURN.fetch_add(1, Ordering::Release);
    // SAFETY: `TURN` is a live, aligned u32.
    unsafe { syscall::futex_wake(TURN.as_ptr(), 1) };
    left
}

/// Sleep until the alarm is due or moved, and once it is due, drop it or end
/// the process as [`alarmreq::ends`] says.
unsafe extern "C" fn keep(_: *mut u8) -> *mut u8 {
    loop {
        // Read before the alarm, so an `alarm` between the two moves it and
        // the wait below returns at once.
        let turn = TURN.load(Ordering::Acquire);
        let mut alarm = ALARM.lock();
        let wait = match alarm.due {
            None => None,
            Some(due) => match due.checked_sub(now()) {
                Some(wait) if wait > 0 => Some(wait),
                _ if alarmreq::ends(act(None).handler) => syscall::exit(ALARM_END),
                _ => {
                    alarm.due = None;
                    None
                }
            },
        };
        drop(alarm);
        // SAFETY: `TURN` is a live, aligned u32.
        unsafe { syscall::futex_wait(TURN.as_ptr(), turn, wait) };
    }
}
