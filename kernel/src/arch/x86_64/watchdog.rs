//! The chipset's watchdog, Intel's TCO or AMD's FCH timer, armed on request
//! and fed from the scheduler pass — the one function an idle CPU and a busy
//! one both run every trip, so **what it proves alive is that some CPU still
//! reaches it**. A panicked machine is reset by the same bound, which is the
//! loop's recovery: logkeeper is dead after a kernel panic, so nothing more
//! could be made durable anyway.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use toyos_tco::fch;
use toyos_tco::{
    Chipset, TCO1_CNT, TCO1_CNT_HALT, TCO1_CNT_RUN, TCO2_STS, TCO_BOOT_STS, TCO_RLD,
    TCO_SECOND_TO_STS, TCO_TMR, TCO_TMR_HLT, TIMER,
};

use crate::arch::pio::{self, Declared, Slot};
use crate::drivers::pci::PciDevice;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;
use crate::sync::Lock;

/// Four, so the shipped 9.6 s bound is fed every 2.4 s. What makes a cadence
/// that long sound is `kernel/CLAUDE.md`'s rule that no disk wait in this kernel
/// can park: a CPU is always on its way back to a scheduler pass.
const FEEDS_PER_BOUND: u64 = 4;

/// What the read-back above the arm says. Whole clauses, because a machine
/// owner and a test read the same line and neither may have to parse a
/// register.
///
/// **`TCO_TMR_HLT` alone answers nothing**: q35 leaves it clear out of reset,
/// so a running timer is the state of a machine nothing has armed as well. The
/// bootloader's arm is `TCO_TMR` holding the bound this tree arms at.
const ARMED_ON_ARRIVAL: &str = "the bootloader had already armed the timer";
const UNARMED_ON_ARRIVAL: &str = "nothing had armed the timer";

/// The TCO block, declared by `init`.
static TCO: Slot = Slot::empty();
/// The FCH's window, mapped by `init`. A lock, because a feed is a
/// read-modify-write that carries `WatchdogRunStopB` as read: one racing
/// `disarm`'s stop would start the timer again.
static FCH: Lock<Option<FchWindow>> = Lock::new(None);
/// Written by `init` on the BSP before any AP exists, so a relaxed load is the whole of the ordering these need.
static ARMED: AtomicBool = AtomicBool::new(false);
static NEXT_FEED: AtomicU64 = AtomicU64::new(u64::MAX);
static FEED_EVERY_NS: AtomicU64 = AtomicU64::new(0);

pub fn init(devices: &[PciDevice]) {
    if !crate::params::watchdog() {
        return;
    }
    for pci in devices {
        if let Some(row) = toyos_tco::chipset(pci.vendor_id(), pci.device_id()) {
            return init_tco(pci, row);
        }
        if fch::carries(pci.vendor_id(), pci.device_id(), pci.revision_id()) {
            return init_fch(pci);
        }
    }
    log!("watchdog: no PCI function here carries a chipset watchdog this kernel knows — not armed");
}

fn init_tco(pci: &PciDevice, row: &Chipset) {
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

    // ICH9 and every PCH since: a 32-byte block.
    let block = match toyos_userbound::Ports::new(port, 0x20).map(|run| pio::declare("the TCO watchdog", run, toyos_userbound::Mediated::ReadOnly)) {
        Some(Ok(block)) => block,
        refused => {
            log!("watchdog: the TCO block at {port:#x} is not this kernel's to drive ({:?}) — not armed", refused.map(|r| r.err()));
            return;
        }
    };
    TCO.set(block);
    arm_tco(row, block, TIMER);
}

fn arm_tco(row: &Chipset, block: Declared, timer: u16) {
    let port = block.ports().first();
    // Read before anything here is written: the bootloader arms the same timer
    // on the same port and hands over a machine already inside the bound, and
    // this is the only place that can say whether it did.
    let cnt = crate::arch::cpu::inw(block.port(TCO1_CNT));
    let tmr = crate::arch::cpu::inw(block.port(TCO_TMR)) & toyos_tco::TMR_MASK;
    let already = cnt & TCO_TMR_HLT == 0 && tmr == TIMER;
    log!(
        "watchdog: TCO1_CNT={cnt:#06x} TCO_TMR={tmr} on arrival, so {}",
        if already { ARMED_ON_ARRIVAL } else { UNARMED_ON_ARRIVAL }
    );

    let stale = crate::arch::cpu::inw(block.port(TCO2_STS));
    if stale & (TCO_SECOND_TO_STS | TCO_BOOT_STS) != 0 {
        log!("watchdog: the last boot ended in a TCO reset (TCO2_STS={stale:#06x})");
        // Cleared, so a reset is reported by the boot after it and not by every
        // boot after it.
        // SAFETY: as the arm below.
        unsafe { crate::arch::cpu::outw(block.port(TCO2_STS), TCO_SECOND_TO_STS | TCO_BOOT_STS) };
    }

    // SAFETY: the block `toyos_tco` answered for the row this machine's own PCI ids matched, declared, and every offset is inside it.
    unsafe {
        crate::arch::cpu::outw(block.port(TCO_TMR), timer);
        crate::arch::cpu::outw(block.port(TCO1_CNT), TCO1_CNT_RUN);
        // Reloading is also what returns the expiry count to zero.
        crate::arch::cpu::outw(block.port(TCO_RLD), 1);
    }

    // Read back: firmware may have set `TCO_LOCK`, which makes `TCO_TMR_HLT` unclearable.
    let cnt = crate::arch::cpu::inw(block.port(TCO1_CNT));
    if cnt & TCO_TMR_HLT != 0 {
        log!("watchdog: {port:#x} kept the timer halted (TCO1_CNT={cnt:#06x}) — not armed");
        return;
    }

    let bound_ms = toyos_tco::bound_of(timer);
    feeding(bound_ms);
    log!(
        "watchdog: {:04x}:{:04x} TCO at {port:#x} TCO_TMR={timer} — this machine resets if no \
         scheduler pass runs for {bound_ms}ms",
        row.vendor,
        row.device
    );
}

/// The FCH's watchdog window, in `fch`'s terms.
struct FchWindow(Mmio);

impl fch::Window for FchWindow {
    fn read(&mut self, register: fch::Register) -> u32 {
        self.0.read_u32(register as u64)
    }

    fn write(&mut self, register: fch::Register, value: u32) {
        self.0.write_u32(register as u64, value)
    }
}

/// Take over the bootloader's arm. **The kernel arms nothing of its own
/// here**: the tick the count is in is `PMx000`'s, which the bootloader
/// selects, and the PM block's page is AML's, which the `acpi` claim's holder
/// is refused once this kernel maps it.
fn init_fch(pci: &PciDevice) {
    let mapped = crate::mm::paging::map_mmio(fch::WDT, fch::WDT_BYTES, MmioPolicy::Uncacheable);
    let mut window = FCH.lock();
    let wdt = window.insert(FchWindow(mapped));
    let found = fch::Found::read(wdt);
    let already = found.armed();
    log!(
        "watchdog: FCH WatchdogControl={:#010x} WatchdogCount={} on arrival, so {}",
        found.control.0,
        found.count,
        if already { ARMED_ON_ARRIVAL } else { UNARMED_ON_ARRIVAL }
    );
    if !already {
        log!("watchdog: the FCH's tick is the bootloader's to select, and it armed nothing — not armed");
        return;
    }
    fch::feed(wdt);

    let bound_ms = fch::bound_of(fch::TIMER);
    feeding(bound_ms);
    log!(
        "watchdog: {:04x}:{:04x} FCH watchdog at {:#x} WatchdogCount={} — this machine resets if \
         no scheduler pass runs for {bound_ms}ms",
        pci.vendor_id(),
        pci.device_id(),
        fch::WDT,
        fch::TIMER
    );
}

fn feeding(bound_ms: u64) {
    FEED_EVERY_NS.store(bound_ms * 1_000_000 / FEEDS_PER_BOUND, Ordering::Relaxed);
    NEXT_FEED.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
}

/// Reload the timer, at most once per feed cadence across every CPU.
pub fn feed(now: u64) {
    // What an unarmed machine pays, and all of it: `NEXT_FEED` is `u64::MAX`.
    let due = NEXT_FEED.load(Ordering::Relaxed);
    if now < due {
        return;
    }
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    let next = now + FEED_EVERY_NS.load(Ordering::Relaxed);
    // A claim, so concurrent CPUs write the register once between them rather than each.
    if NEXT_FEED.compare_exchange(due, next, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    match TCO.get() {
        // SAFETY: as `arm_tco`'s, and a reload racing `disarm` restarts nothing — `hw/acpi/ich9_tco.c:146` reloads only while `TCO_TMR_HLT` is clear, and the PCH half is unverified.
        Some(block) => unsafe { crate::arch::cpu::outw(block.port(TCO_RLD), 1) },
        None => {
            let mut window = FCH.lock();
            // Asked again under the lock `disarm` stops the timer under.
            if ARMED.load(Ordering::Relaxed) {
                fch::feed(window.as_mut().expect("watchdog: armed with neither a TCO block nor an FCH window"));
            }
        }
    }
}

pub fn disarm() {
    if !ARMED.swap(false, Ordering::Relaxed) {
        return;
    }
    NEXT_FEED.store(u64::MAX, Ordering::Relaxed);
    match TCO.get() {
        // SAFETY: as `arm_tco`'s.
        Some(block) => unsafe { crate::arch::cpu::outw(block.port(TCO1_CNT), TCO1_CNT_HALT) },
        // Before the reset that follows, which the FCH's block survives.
        None => fch::stop(FCH.lock().as_mut().expect("watchdog: armed with neither a TCO block nor an FCH window")),
    }
    log!("watchdog: disarmed");
}
