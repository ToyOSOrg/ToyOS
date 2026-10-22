//! The machine's two ends this kernel performs, a reset and a power-off, each
//! the architecture's own (`arch::power`).
//!
//! **No reset this kernel performs leaves a claimed function mastering.**
//! [`reset_now`] and [`shutdown`] are the only two places this kernel ends the
//! machine, and each stops every function a claim holds
//! ([`crate::pcidev::stop_before_reset`]) before it does — which is what
//! makes that a property of the reset rather than of whoever asked for one,
//! and what a third caller gets without knowing it is owed. Resets this
//! kernel does not perform — a TCO or firmware watchdog, a triple fault,
//! power loss — are outside it and always will be.
//!
//! **Nothing an end says is logged after the console's last drain.** The
//! stop holds the console's wire (`log::console::StopWire`), or writes over
//! the holder that kept it, and its drain here is the log ring's last reader
//! for the console: a record committed past it reaches no wire. What an end says
//! through the ring it says above the drain (`arch::power::settle`);
//! `arch::power::off` and `arch::power::reset` log nothing, and a panic in
//! either drains for itself. The black box's page is another reader and an
//! earlier one: its tail is sealed before either end is entered
//! (`log::seal_tail`), so what `settle` says is on the console and in no page.

use crate::log::console::StopWire;

/// Whether this machine has a reset this kernel can perform.
pub fn can_reboot() -> bool {
    crate::arch::power::can_reset()
}

/// Why this machine has no power-off this kernel performs, where it has none.
pub fn shutdown_refused() -> Option<&'static str> {
    crate::arch::power::off_refused()
}

/// Return the machine to firmware.
pub fn reboot(mut wire: StopWire) -> ! {
    wire.drain();
    // Kernel-internal, so a bug rather than a machine quiesced and then left halted quietly.
    assert!(can_reboot(), "reboot: no reset, and the caller did not ask can_reboot() first");
    reset_now()
}

/// Reset the machine and do nothing else.
///
/// **[`reboot`] is not reachable from a wedge**, which is why this exists
/// beside it: that path is the stop's, which waits on `klogd` and the
/// console's registers, and a `BackendGuard` masks interrupts for its whole
/// life — so a CPU stuck inside one holds what the stop would wait for, on
/// exactly the boots `crate::deadline` exists for. This takes no lock.
///
/// A machine with no reset halts here rather than returning: the caller has
/// already sealed why, and holding is what such a machine has always done.
pub fn reset_now() -> ! {
    // **Here and not at either caller**: the stop is registers and nothing
    // else, written for the panic path, so it takes no lock and allocates
    // nothing, which is the only kind of call a wedge may make.
    crate::pcidev::stop_before_reset();
    crate::arch::power::reset()
}

/// Power the machine off. The caller asked [`shutdown_refused`] first.
pub fn shutdown(stopping: crate::quiesce::Stopping, mut wire: StopWire) -> ! {
    // Above the drain, because it logs.
    let settled = crate::arch::power::settle(stopping);
    // Nothing drains the log ring after this point, and nothing below logs.
    wire.drain();
    crate::pcidev::stop_before_reset();
    crate::arch::power::off(settled)
}
