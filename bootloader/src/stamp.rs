//! The head of every line the loader says: the log's one head
//! (`toyos_abi::log::Head`), whose time is the seconds since the CPU's counter
//! last started — power-on, or the reset since — at the rate the CPU states
//! ([`crate::arch::counter_hz`]). The kernel's records and every program's
//! line count from the same zero, so a loader line and the kernel's first
//! record are placed against each other. No wall clock.
//!
//! A CPU that states no rate gets lines whose time is `toyos_abi::log::UNTIMED`,
//! and [`start`]'s caller says so once.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use toyos_abi::log::{Head, Severity, LOADER};

/// The counter's rate; zero until [`start`], and for a CPU that states none.
static HZ: AtomicU64 = AtomicU64::new(0);

/// Read the rate stamps are converted at, and return it, or `None` where the
/// CPU states none.
pub fn start() -> Option<u64> {
    let hz = crate::arch::counter_hz();
    HZ.store(hz.unwrap_or(0), Ordering::Relaxed);
    hz
}

/// Now, as a line's head. The loader runs on the boot CPU alone, the kernel's
/// `cpu0`.
pub fn now() -> Head<'static> {
    let hz = HZ.load(Ordering::Relaxed);
    let at_ns = (hz != 0).then(|| (u128::from(crate::arch::counter()) * 1_000_000_000 / u128::from(hz)) as u64);
    Head { wall: "", at_ns, cpu: Some(0), who: LOADER, severity: Severity::Info, tid: 0, pid: None }
}

/// One line, stamped, to the console and then to `loader.log`.
pub fn say(args: fmt::Arguments) {
    let head = now();
    crate::efi::print(format_args!("{head} {args}\n"));
    crate::loaderlog::line(format_args!("{head} {args}"));
}
