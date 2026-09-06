//! The boot parameters a **shipping** kernel answers to. An actuator is the
//! other kind of token and is test-only, so a kernel built without them refuses
//! every one it is handed; a name here is claimed before that table sees it,
//! and is the only way an image the owner flashes asks for anything. Spelled
//! once, in [`PARAMS`], which `src/build.rs` reads.

use core::sync::atomic::{AtomicBool, Ordering};

pub const PARAMS: &[&str] = &["watchdog"];

const WATCHDOG: &str = PARAMS[0];

static WATCHDOG_NAMED: AtomicBool = AtomicBool::new(false);

pub fn init(cmdline: &str) {
    for token in toyos_abi::boot::actuators(cmdline) {
        if token == WATCHDOG {
            WATCHDOG_NAMED.store(true, Ordering::Relaxed);
        }
    }
}

pub fn claims(token: &str) -> bool {
    PARAMS.contains(&token)
}

pub fn watchdog() -> bool {
    WATCHDOG_NAMED.load(Ordering::Relaxed)
}
