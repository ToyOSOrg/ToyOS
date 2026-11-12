//! The boot deadline: the one bound on this machine that nothing running on it
//! can hold off.
//!
//! Every other bound a boot runs under is fed or checked by the thing it
//! bounds: the firmware's watchdog dies at `ExitBootServices`, the chipset's TCO
//! does not count on every PCH, the runner's job bound is a check made *between*
//! jobs, and the panic path's bound exists only once something has panicked. A
//! kernel that stops making progress without panicking is bounded by none of
//! them, and on a machine with no power switch that is a hand on the button.
//!
//! So this one is armed off the parameter line and polled from the timer
//! interrupt entry, on every CPU, in both rings. Its whole requirement is that
//! some CPU still takes an interrupt: [`poll`] reads two atomics and the CPU's
//! counter, takes no lock, allocates nothing and calls nothing that can, so whatever a
//! wedged path is holding is not on this path.
//!
//! # What it does not cover, stated rather than implied
//!
//! - **The span before `clock::init`,** which no bound in this file or in
//!   [`crate::hardlockup`] reaches: no counter period to convert a bound with, and
//!   no counter armed. A kernel that wedges there still needs a hand; one that
//!   panics there is ended by `crate::panic_reboot`'s bound.
//! - **A machine on which no CPU takes an interrupt at all.** Nothing polled
//!   from a running CPU can cover that; **that half is [`crate::hardlockup`]**,
//!   sampled by an NMI off a performance counter. The two are armed off this one
//!   parameter and compose **by scope and not by machine state**: this bound is
//!   the whole machine's and that one is any *single CPU*'s, so a machine with
//!   one deaf core is ended by that one, at the earlier bound. Whichever fires
//!   takes the machine's one reset through [`claim_the_reset`].
//! - **A panic in progress**, which is not a gap but a stand-down:
//!   `panic::stop_the_machine` calls [`stand_down`] before a fatal path holds
//!   the panel, so a panic report is never cut short by an expiry.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// The bound this boot was armed with, in milliseconds, or 0 for a boot that
/// named none. Written on the BSP before any AP exists.
static BOUND_MS: AtomicU64 = AtomicU64::new(0);

/// The `cpu::counter` reading the machine is reset at, or 0 while unarmed.
///
/// **A counter reading and not a nanosecond one**, because `clock::nanos_since_boot`'s
/// multiply-and-divide is out of line and this is read on every timer tick of
/// every CPU.
static AT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether a CPU has taken the expiry. One machine, one reset.
static FIRED: AtomicBool = AtomicBool::new(false);

/// Take this machine's one reset, or `false` where another CPU already holds
/// it. Shared with [`crate::hardlockup`] and `crate::panic_reboot`'s bound:
/// three bounds, one reset.
pub fn claim_the_reset() -> bool {
    !FIRED.swap(true, Relaxed)
}

/// Stand this bound down for the rest of the machine's life.
///
/// Called from `panic::stop_the_machine` beside [`crate::hardlockup::stand_down`]:
/// from there this machine holds a panic report under a bound of its own, and an
/// expiry would reset it out from under its reader. Disarms rather than latching a
/// second flag, so [`poll`] stays one relaxed load.
pub fn stand_down() {
    AT_COUNTER.store(0, Relaxed);
}

/// Whether this boot runs under a bound at all — this one or
/// [`crate::hardlockup`]'s, since one parameter arms both.
///
/// **What the idle path asks before it re-arms a one-shot.** Both bounds rest on
/// some CPU taking a timer interrupt, and a CPU woken by an IPI and held in Ring
/// 0 takes none; on a boot that named no bound that re-arm is timer
/// register traffic on every wake that buys nothing.
pub fn armed() -> bool {
    AT_COUNTER.load(Relaxed) != 0
}

/// Read the bound off the parameter line.
///
/// Beside `params::init`, on the same `&str` and for the same reason: the
/// parameter is in no reserved region, so `mm::init` may hand that memory out
/// and nothing may hold a borrow of it past here. The bound is only turned into
/// a deadline by [`start`], once there is a clock to measure one against.
pub fn claim(cmdline: &str) {
    match toyos_tco::deadline_in(cmdline) {
        None => {}
        Some(Ok(ms)) => BOUND_MS.store(ms, Relaxed),
        // Refused by name rather than booted unarmed: an image whose bound
        // nothing can read is an image with no deadline, and a boot that
        // silently had none is the case this whole mechanism exists to end.
        Some(Err(value)) => panic!(
            "{}{value}: the boot deadline is a number of milliseconds",
            toyos_tco::DEADLINE_PARAM
        ),
    }
}

/// Turn the bound into a deadline, once the counter's period is known.
///
/// Called after `clock::init` and the timer's arm, which are the two
/// things [`poll`] needs: a period to convert with, and something that fires.
pub fn start() {
    let ms = BOUND_MS.load(Relaxed);
    if ms == 0 {
        return;
    }
    AT_COUNTER.store(crate::clock::counter_deadline(ms.saturating_mul(1_000_000)), Relaxed);
    log!("boot deadline: {ms} ms, after which this kernel writes the reset register itself");
    // The other half of the same parameter: what one CPU gets to take no
    // interrupt at all, which this poll cannot see because it is not running.
    // Written from here because everything that module does is reached from an
    // NMI, and none of it may say a word.
    log!("{}", crate::hardlockup::start(ms));
}

pub fn poll() {
    let at = AT_COUNTER.load(Relaxed);
    if at == 0 || crate::arch::cpu::counter() < at {
        return;
    }
    expire()
}

/// Stop what is doing DMA, and hand the machine back.
///
/// Runs inside an interrupt entry on whichever CPU noticed: no lock, no
/// allocation, nothing that can block, since every lock in this kernel is a
/// thing the wedge may be holding.
fn expire() -> ! {
    if !claim_the_reset() {
        // Another CPU is already resetting.
        crate::arch::cpu::halt();
    }
    #[cfg(feature = "mask-windows")]
    crate::windows::stand_down();
    crate::power::reset_now()
}
