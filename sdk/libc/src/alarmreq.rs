//! What `alarm` answers and arms, in nanoseconds of the monotonic clock, and
//! what a due one does. It reads nothing but what it is handed, so the host
//! tests it (`toyos-libc-copies`).

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// `signal.h`'s `SIGALRM`, and its `SIG_DFL` and `SIG_IGN` as addresses.
pub(crate) const SIGALRM: i32 = 14;
pub(crate) const SIG_DFL: usize = 0;
pub(crate) const SIG_IGN: usize = 1;

/// Whether an alarm due under `SIGALRM`'s disposition `handler` ends the
/// process: it does unless `SIGALRM` is ignored, since no handler runs
/// (`issues/an-alarm-reaches-no-sigalrm-handler.md`).
pub(crate) fn ends(handler: usize) -> bool {
    handler != SIG_IGN
}

/// When an alarm of `seconds` armed at `now` is due; none for 0, which
/// disarms.
pub(crate) fn due(now: u64, seconds: u32) -> Option<u64> {
    // No overflow: `u32::MAX` seconds are under 2^62 nanoseconds, and `now`
    // counts from boot.
    (seconds != 0).then(|| now + u64::from(seconds) * NANOS_PER_SEC)
}

/// The seconds an alarm due at `due` has left at `now`, rounded up: one that
/// has any time left answers at least 1, and none, or one already due, 0.
pub(crate) fn left(due: Option<u64>, now: u64) -> u32 {
    let Some(due) = due.filter(|&due| due > now) else { return 0 };
    // At most the `u32` seconds `due` was armed with.
    (due - now).div_ceil(NANOS_PER_SEC) as u32
}
