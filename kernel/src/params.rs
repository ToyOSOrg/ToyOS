//! The boot parameters a **shipping** kernel answers to. An actuator is the
//! other kind of token and is test-only, so a kernel built without them refuses
//! every one it is handed; a name here is claimed before that table sees it,
//! and is the only way an image the owner flashes asks for anything.

use core::sync::atomic::{AtomicBool, Ordering};

/// Each parameter beside the flag it sets, so a name cannot be claimed and then handled by nothing.
pub const PARAMS: &[(&str, &AtomicBool)] =
    &[(toyos_tco::PARAM, &WATCHDOG_NAMED), ("early-panel", &EARLY_PANEL_NAMED)];

static WATCHDOG_NAMED: AtomicBool = AtomicBool::new(false);
static EARLY_PANEL_NAMED: AtomicBool = AtomicBool::new(false);
static PARSED: AtomicBool = AtomicBool::new(false);

pub fn init(cmdline: &str) {
    for token in toyos_abi::boot::actuators(cmdline) {
        if let Some((_, named)) = PARAMS.iter().find(|(name, _)| *name == token) {
            named.store(true, Ordering::Relaxed);
        }
    }
    PARSED.store(true, Ordering::Relaxed);
}

pub fn claims(token: &str) -> bool {
    PARAMS.iter().any(|(name, _)| *name == token)
}

pub fn watchdog() -> bool {
    WATCHDOG_NAMED.load(Ordering::Relaxed)
}

/// Every record before this ran repaints the panel: the boot parameter it reads
/// is dereferenced through a mapping nothing has checked, and a fault there
/// leaves no channel at all. After it, only a boot that named `early-panel`.
pub fn early_panel() -> bool {
    !PARSED.load(Ordering::Relaxed) || EARLY_PANEL_NAMED.load(Ordering::Relaxed)
}
