//! The boot parameters a **shipping** kernel answers to. An actuator is the
//! other kind of token and is test-only, so a kernel built without them refuses
//! every one it is handed; a name here is claimed before that table sees it,
//! and is the only way an image the owner flashes asks for anything.
//!
//! Two kinds live here: a flag, which is a name [`PARAMS`] matches whole, and a
//! parameter carrying a value after it, which [`claims`] matches as a prefix
//! and no table holds.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::Lock;

/// Each parameter beside the flag it sets, so a name cannot be claimed and then handled by nothing.
pub const PARAMS: &[(&str, &AtomicBool)] =
    &[(toyos_tco::PARAM, &WATCHDOG_NAMED), ("early-panel", &EARLY_PANEL_NAMED)];

static WATCHDOG_NAMED: AtomicBool = AtomicBool::new(false);
static EARLY_PANEL_NAMED: AtomicBool = AtomicBool::new(false);
static PARSED: AtomicBool = AtomicBool::new(false);

/// [`toyos_logstream::PARAM`]'s value, **copied** and not borrowed.
///
/// The parameter line is in memory no reserved region covers, so `mm::init` may
/// hand it out; nothing may hold a borrow of it past [`init`]. There is also no
/// allocator yet, so the copy goes into a fixed buffer rather than a `String`.
static LOG_STREAM: Lock<([u8; toyos_logstream::MAX_VALUE_BYTES], usize)> =
    Lock::new(([0; toyos_logstream::MAX_VALUE_BYTES], 0));

pub fn init(cmdline: &str) {
    for token in toyos_abi::boot::actuators(cmdline) {
        if let Some((_, named)) = PARAMS.iter().find(|(name, _)| *name == token) {
            named.store(true, Ordering::Relaxed);
        }
    }
    if let Some(value) = toyos_logstream::value_in(cmdline) {
        // A value this buffer cannot hold is refused whole rather than
        // truncated: half an address is an address, and it is somebody else's.
        if value.len() > toyos_logstream::MAX_VALUE_BYTES {
            crate::log!(
                "log-stream: {}{value:?} is {} bytes and an address is at most {}",
                toyos_logstream::PARAM,
                value.len(),
                toyos_logstream::MAX_VALUE_BYTES
            );
        } else {
            let mut held = LOG_STREAM.lock();
            held.0[..value.len()].copy_from_slice(value.as_bytes());
            held.1 = value.len();
        }
    }
    PARSED.store(true, Ordering::Relaxed);
}

/// Whether this kernel handles `token` itself, which is what stops
/// `actuator::init` refusing it as a name it does not know.
///
/// **Neither parameter that carries a value is in [`PARAMS`]**, which is a
/// table of flags matched whole; both are prefixes matched here. The black-box
/// page's address is not read by [`init`] either — it comes out of the raw
/// buffer in `kernel_main`'s first statements (`crate::blackbox::arm`), because
/// a panic before [`init`] runs still has to be able to seal. The log stream's
/// address is read by [`init`], because nothing before it needs one.
pub fn claims(token: &str) -> bool {
    PARAMS.iter().any(|(name, _)| *name == token)
        || token.starts_with(toyos_blackbox::PARAM)
        || token.starts_with(toyos_logstream::PARAM)
}

/// Where this boot streams its records, for the one hop the kernel makes with
/// it: into `/system/bin/init`'s environment, which every program inherits and
/// `logd` is the one program endowed to act on.
///
/// A `String` and not a borrow, because the buffer behind it is locked and the
/// bytes it copied are gone from the parameter line by now. Called once, after
/// the allocator exists.
pub fn log_stream() -> Option<alloc::string::String> {
    let held = LOG_STREAM.lock();
    if held.1 == 0 {
        return None;
    }
    // Written from a `&str`, so this cannot fail; a refusal here would still be
    // an empty stream rather than a boot that dies over a log's address.
    core::str::from_utf8(&held.0[..held.1]).ok().map(alloc::string::ToString::to_string)
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
