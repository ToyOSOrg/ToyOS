use super::*;
use common::clock::*;
use std::sync::atomic::Ordering;

pub fn stage_suspend(how_long: Duration) {
    STAGED.store(how_long.as_millis() as u64, Ordering::SeqCst);
}

pub fn self_check() -> Result<(), String> {
    let quiet = mark();
    std::thread::sleep(Duration::from_millis(20));
    let idle = quiet.suspended();
    if idle > Duration::from_millis(1) {
        return Err(format!("a host that did not stop reports {idle:?} of suspend"));
    }
    if idle >= SUSPENDED_AT_LEAST {
        return Err("an ordinary 20 ms of waiting reads as a suspend".to_string());
    }

    let across = mark();
    stage_suspend(Duration::from_secs(90));
    // Read once and reset: every reading takes the clocks afresh, so a second
    // one after the reset would be of a host that never stopped.
    let seen = across.suspended();
    stage_suspend(Duration::ZERO);

    if !(Duration::from_secs(89)..=Duration::from_secs(91)).contains(&seen) {
        return Err(format!("staged 90 s of suspend and the detector saw {seen:?}"));
    }
    if seen < SUSPENDED_AT_LEAST {
        return Err("90 s of host suspend did not invalidate the interval".to_string());
    }

    // And the reading is of the *interval*, not of the process: a mark taken
    // after the host came back must be clean, or every later test in the run
    // would inherit one lid closing.
    let after = mark();
    if after.suspended() >= SUSPENDED_AT_LEAST {
        return Err("a suspend before an interval invalidated it anyway".to_string());
    }
    Ok(())
}
