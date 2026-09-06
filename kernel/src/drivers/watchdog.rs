//! The chipset's TCO watchdog: armed on request, fed from the scheduler pass.
//!
//! **What it proves alive is that some CPU still reaches `sched::driver::pass`**,
//! the one function an idle CPU and a busy one both run every trip. A machine
//! where that stops for the whole bound is wedged by the only definition this
//! kernel has, and the chipset resets it back to firmware — where, on the T14,
//! the default boot entry is Linux and the loop can go and read the log.
//!
//! Armed only when the boot parameter names it: a watchdog nobody asked for is
//! a machine that reboots under its owner. `toyos-tco` decides which port and
//! which timer value; everything that touches the machine is here.
//!
//! **Only QEMU's q35 row is judged.** A row for another chipset is reachable on
//! that machine and nowhere in this harness.

use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use toyos_tco::{Chipset, TCO1_CNT, TCO1_CNT_HALT, TCO1_CNT_RUN, TCO1_STS, TCO_RLD, TCO_TIMEOUT, TCO_TMR};

use crate::drivers::pci::PciDevice;
use crate::log;
use crate::time::Duration;

/// What the boot parameter asks for. One bound, not a per-machine choice: the
/// chipset ignores anything under two ticks and holds ten bits of them.
const BOUND: Duration = Duration::from_secs(300);
/// The bound the guest tests arm, short enough that a run is not five minutes.
const FAST_BOUND: Duration = Duration::from_secs(3);
/// Feeds fit in a bound before the chipset acts, so three may be missed.
const FEEDS_PER_BOUND: u64 = 4;

/// The port `TCO_RLD` is at, or zero on a machine that armed nothing.
static PORT: AtomicU16 = AtomicU16::new(0);
/// When the next feed is due; `u64::MAX` while nothing is armed.
static NEXT_FEED: AtomicU64 = AtomicU64::new(u64::MAX);
static FEED_EVERY_NS: AtomicU64 = AtomicU64::new(0);

/// Arm the watchdog if the boot parameter named it and this machine has a row.
pub fn init(devices: &[PciDevice]) {
    if !crate::actuator::watchdog() {
        return;
    }
    let bound = if crate::actuator::watchdog_fast() { FAST_BOUND } else { BOUND };
    let Some(timer) = toyos_tco::timer_for(bound.millis()) else {
        log!("watchdog: no TCO timer reaches {} — not armed", bound);
        return;
    };

    let Some((pci, row)) = devices
        .iter()
        .find_map(|d| toyos_tco::chipset(d.vendor_id(), d.device_id()).map(|row| (d, row)))
    else {
        log!("watchdog: no PCI function here carries a TCO block this kernel knows — not armed");
        return;
    };

    let port = match row.port(
        pci.read_config_u32(u64::from(row.base_reg)),
        pci.read_config_u32(u64::from(row.enable.0)),
    ) {
        Ok(port) => port,
        Err(why) => {
            log!("watchdog: {:04x}:{:04x} names no TCO port ({why:?})", row.vendor, row.device);
            return;
        }
    };

    arm(row, port, timer);
}

/// Order is load-bearing: the count and the stale status first, then the
/// register that starts the timer, then the first feed.
fn arm(row: &Chipset, port: u16, timer: u16) {
    // SAFETY: `port` is `toyos_tco`'s answer for the row this machine's own PCI ids matched, and every offset is inside that row's block.
    unsafe {
        crate::arch::cpu::outw(port + TCO_TMR, timer);
        // Write-one-to-clear, so an expiry latched before this boot does not count against the first period.
        crate::arch::cpu::outw(port + TCO1_STS, TCO_TIMEOUT);
        crate::arch::cpu::outw(port + TCO1_CNT, TCO1_CNT_RUN);
        crate::arch::cpu::outw(port + TCO_RLD, 1);
    }

    let bound_ms = toyos_tco::bound_of(timer);
    FEED_EVERY_NS.store(bound_ms * 1_000_000 / FEEDS_PER_BOUND, Ordering::Relaxed);
    NEXT_FEED.store(0, Ordering::Relaxed);
    PORT.store(port, Ordering::Release);
    log!(
        "watchdog: {:04x}:{:04x} TCO at {port:#x} TCO_TMR={timer} — this machine resets if no \
         scheduler pass runs for {bound_ms}ms",
        row.vendor,
        row.device
    );
}

/// Reload the timer, at most once per feed cadence across every CPU.
///
/// On the pass path, so what an unarmed machine pays is this load and this
/// compare: `NEXT_FEED` starts at `u64::MAX` and answers without a port write.
pub fn feed(now: u64) {
    // The wedge this exists to survive, staged: the machine goes on running and
    // the chipset stops hearing from it.
    if crate::actuator::watchdog_starve() {
        return;
    }
    let due = NEXT_FEED.load(Ordering::Relaxed);
    if now < due {
        return;
    }
    let next = now + FEED_EVERY_NS.load(Ordering::Relaxed);
    // A claim, so concurrent CPUs write the port once between them rather than each.
    if NEXT_FEED.compare_exchange(due, next, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    let port = PORT.load(Ordering::Acquire);
    if port == 0 {
        return;
    }
    // SAFETY: non-zero only after `arm` published the port `toyos_tco` answered for this machine's chipset row.
    unsafe { crate::arch::cpu::outw(port + TCO_RLD, 1) };
}

/// Halt the timer, so a machine on its way down is not reset on the way.
///
/// Both power paths call this before they quiesce: the sync and the log's
/// durable wait can each outlast a feed cadence, and the pass that would have
/// fed is not going to run again.
pub fn disarm() {
    let port = PORT.swap(0, Ordering::AcqRel);
    if port == 0 {
        return;
    }
    NEXT_FEED.store(u64::MAX, Ordering::Relaxed);
    // SAFETY: as `feed`, and the swap means only the caller that took the port writes it.
    unsafe { crate::arch::cpu::outw(port + TCO1_CNT, TCO1_CNT_HALT) };
    log!("watchdog: disarmed");
}
