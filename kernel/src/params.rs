//! The boot parameters a **shipping** kernel answers to. An actuator is the
//! other kind of token and is test-only, so a kernel built without them refuses
//! every one it is handed; a name here is claimed before that table sees it,
//! and is the only way an image the owner flashes asks for anything.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Each parameter beside the flag it sets, so a name cannot be claimed and then handled by nothing.
pub const PARAMS: &[(&str, &AtomicBool)] =
    &[(toyos_tco::PARAM, &WATCHDOG_NAMED), ("early-panel", &EARLY_PANEL_NAMED)];

static WATCHDOG_NAMED: AtomicBool = AtomicBool::new(false);
static EARLY_PANEL_NAMED: AtomicBool = AtomicBool::new(false);
static PARSED: AtomicBool = AtomicBool::new(false);

/// The black-box page's address, or 0 for a boot whose loader claimed none.
///
/// **The one parameter that carries a value**, and the reason it does: the page
/// is the loader's allocation, so whether there is one is a fact only the loader
/// has. It used to be read out of the memory map, off a UEFI memory type of this
/// project's own — until the owner's firmware stopped returning from
/// `ExitBootServices` with one of those in the map it was handed.
static BLACKBOX_PAGE: AtomicU64 = AtomicU64::new(0);

pub fn init(cmdline: &str) {
    for token in toyos_abi::boot::actuators(cmdline) {
        if let Some((_, named)) = PARAMS.iter().find(|(name, _)| *name == token) {
            named.store(true, Ordering::Relaxed);
        }
        if let Some(at) = toyos_blackbox::address_of(token) {
            BLACKBOX_PAGE.store(at, Ordering::Relaxed);
        }
    }
    PARSED.store(true, Ordering::Relaxed);
}

/// Whether this kernel handles `token` itself, which is what stops
/// `actuator::init` refusing it as a name it does not know.
pub fn claims(token: &str) -> bool {
    PARAMS.iter().any(|(name, _)| *name == token) || token.starts_with(toyos_blackbox::PARAM)
}

/// Where the loader put the black-box page, or `None` where it claimed none.
/// Read before `mm::init`, which is what keeps the page out of the allocator.
pub fn blackbox_page() -> Option<u64> {
    let at = BLACKBOX_PAGE.load(Ordering::Relaxed);
    (at != 0).then_some(at)
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
