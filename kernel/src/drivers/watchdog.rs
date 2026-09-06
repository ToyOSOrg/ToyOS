//! The chipset's TCO watchdog, armed on request and fed from the scheduler
//! pass — the one function an idle CPU and a busy one both run every trip, so
//! **what it proves alive is that some CPU still reaches it**. A panicked
//! machine is reset by the same bound, which is the loop's recovery: logd is
//! dead after a kernel panic, so nothing more could be made durable anyway.

use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use toyos_tco::{
    Chipset, TCO1_CNT, TCO1_CNT_HALT, TCO1_CNT_RUN, TCO2_STS, TCO_BOOT_STS, TCO_RLD,
    TCO_SECOND_TO_STS, TCO_TMR, TCO_TMR_HLT,
};

use crate::drivers::pci::PciDevice;
use crate::log;
use crate::time::Duration;

const BOUND: Duration = Duration::from_secs(300);
const FAST_BOUND: Duration = Duration::from_secs(3);
const FEEDS_PER_BOUND: u64 = 4;

/// Neither bound is a value this kernel can fail to have.
const TIMER: u16 = match toyos_tco::timer_for(BOUND.millis()) {
    Some(timer) => timer,
    None => panic!("the shipped bound reaches no TCO timer"),
};
const FAST_TIMER: u16 = match toyos_tco::timer_for(FAST_BOUND.millis()) {
    Some(timer) => timer,
    None => panic!("the fast bound reaches no TCO timer"),
};

/// When `tco-starve` starts starving: boot is long done by here, so a judge measures a reset after starvation and never a race with it.
const STARVE_AFTER: Duration = Duration::from_secs(5);

/// Written by `init` on the BSP before any AP exists, so a relaxed load is the whole of the ordering these need.
static PORT: AtomicU16 = AtomicU16::new(0);
static NEXT_FEED: AtomicU64 = AtomicU64::new(u64::MAX);
static FEED_EVERY_NS: AtomicU64 = AtomicU64::new(0);

pub fn init(devices: &[PciDevice]) {
    if !crate::params::watchdog() {
        return;
    }
    let timer = if crate::actuator::watchdog_fast() { FAST_TIMER } else { TIMER };

    let Some((pci, row)) = devices
        .iter()
        .find_map(|d| toyos_tco::chipset(d.vendor_id(), d.device_id()).map(|row| (d, row)))
    else {
        log!("watchdog: no PCI function here carries a TCO block this kernel knows — not armed");
        return;
    };

    let base = pci.read_config_u32(u64::from(row.base_reg));
    // One read where a chipset keeps both in one register, which q35 does.
    let enable = if row.enable.reg == row.base_reg {
        base
    } else {
        pci.read_config_u32(u64::from(row.enable.reg))
    };
    let port = match row.port(base, enable) {
        Ok(port) => port,
        Err(why) => {
            log!("watchdog: {:04x}:{:04x} names no TCO port ({why:?})", row.vendor, row.device);
            return;
        }
    };

    arm(row, port, timer);
}

fn arm(row: &Chipset, port: u16, timer: u16) {
    let stale = crate::arch::cpu::inw(port + TCO2_STS);
    if stale & (TCO_SECOND_TO_STS | TCO_BOOT_STS) != 0 {
        log!("watchdog: the last boot ended in a TCO reset (TCO2_STS={stale:#06x})");
        // Cleared, or the latches are sticky across resets and every later boot
        // reports this one. They are write-one-to-clear on the PCH and masked
        // out of QEMU's own store (`ich9_tco.c:167`), so this word clears them
        // either way — and in QEMU zeroes the rest of the register with them.
        // SAFETY: as the arm below.
        unsafe { crate::arch::cpu::outw(port + TCO2_STS, TCO_SECOND_TO_STS | TCO_BOOT_STS) };
    }

    // SAFETY: `port` is `toyos_tco`'s answer for the row this machine's own PCI ids matched, and every offset is inside that row's block.
    unsafe {
        crate::arch::cpu::outw(port + TCO_TMR, timer);
        crate::arch::cpu::outw(port + TCO1_CNT, TCO1_CNT_RUN);
        // Reloading is also what returns the expiry count to zero.
        crate::arch::cpu::outw(port + TCO_RLD, 1);
    }

    // Read back: firmware may have set `TCO_LOCK`, which makes `TCO_TMR_HLT` unclearable.
    let cnt = crate::arch::cpu::inw(port + TCO1_CNT);
    if cnt & TCO_TMR_HLT != 0 {
        log!("watchdog: {port:#x} kept the timer halted (TCO1_CNT={cnt:#06x}) — not armed");
        return;
    }

    let bound_ms = toyos_tco::bound_of(timer);
    FEED_EVERY_NS.store(bound_ms * 1_000_000 / FEEDS_PER_BOUND, Ordering::Relaxed);
    NEXT_FEED.store(0, Ordering::Relaxed);
    PORT.store(port, Ordering::Relaxed);
    log!(
        "watchdog: {:04x}:{:04x} TCO at {port:#x} TCO_TMR={timer} — this machine resets if no \
         scheduler pass runs for {bound_ms}ms",
        row.vendor,
        row.device
    );
}

/// Reload the timer, at most once per feed cadence across every CPU.
pub fn feed(now: u64) {
    // What an unarmed machine pays, and all of it: `NEXT_FEED` is `u64::MAX`.
    let due = NEXT_FEED.load(Ordering::Relaxed);
    if now < due {
        return;
    }
    let port = PORT.load(Ordering::Relaxed);
    if port == 0 {
        return;
    }
    if crate::actuator::watchdog_starve() && now >= STARVE_AFTER.nanos() {
        return;
    }
    let next = now + FEED_EVERY_NS.load(Ordering::Relaxed);
    // A claim, so concurrent CPUs write the port once between them rather than each.
    if NEXT_FEED.compare_exchange(due, next, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    // SAFETY: as `arm`'s, and a reload racing `disarm` restarts nothing — `hw/acpi/ich9_tco.c:146` reloads only while `TCO_TMR_HLT` is clear, and the PCH half is unverified.
    unsafe { crate::arch::cpu::outw(port + TCO_RLD, 1) };
}

pub fn disarm() {
    let port = PORT.swap(0, Ordering::Relaxed);
    if port == 0 {
        return;
    }
    NEXT_FEED.store(u64::MAX, Ordering::Relaxed);
    // SAFETY: as `arm`'s, on the port this call took out of `PORT`.
    unsafe { crate::arch::cpu::outw(port + TCO1_CNT, TCO1_CNT_HALT) };
    log!("watchdog: disarmed");
}
