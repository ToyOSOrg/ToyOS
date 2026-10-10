//! The local APIC, in the mode `control_regs` declared for every CPU: x2APIC
//! through MSRs where CPUID offers it, xAPIC through its MMIO page where it
//! does not. Every register is reached through [`Reg`], which is the only
//! place the two modes differ but for the ICR's destination.
//!
//! Sections cited here are the Intel SDM's, Vol. 3A order 325384-093US,
//! chapter 13, and the AMD APM's, Vol. 2 publication 24593 rev. 3.45,
//! chapter 16.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::control_regs::{self, ApicMode};
use super::{cpu, percpu};
use crate::hw::MIN_ONE_SHOT;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::{DirectMap, Mmio};
use crate::time::{Delay, Duration};

/// A local APIC register by its xAPIC MMIO offset (SDM Table 13-1, APM Table
/// 16-2); its x2APIC MSR is `0x800 + offset / 16` (SDM §13.12.1.2, APM
/// §16.11.1). Only the constants below exist, so every register named is an
/// architectural one touching no memory or control transfer.
#[derive(Clone, Copy)]
struct Reg(u32);

impl Reg {
    const ID: Reg = Reg(0x20);
    const EOI: Reg = Reg(0xB0);
    const SVR: Reg = Reg(0xF0);
    /// The ICR's command half; in x2APIC the whole 64-bit ICR (SDM §13.6.1,
    /// §13.12.9; APM §16.5, §16.13).
    const ICR: Reg = Reg(0x300);
    /// The ICR's destination half, xAPIC only: x2APIC has no MSR 831H.
    const ICR_HIGH: Reg = Reg(0x310);
    const LVT_TIMER: Reg = Reg(0x320);
    /// The performance-monitoring counters' LVT entry.
    const LVT_PMC: Reg = Reg(0x340);
    const TIMER_INIT: Reg = Reg(0x380);
    const TIMER_CURRENT: Reg = Reg(0x390);
    const TIMER_DIVIDE: Reg = Reg(0x3E0);

    /// The in-service word holding `vector`'s bit, one of eight at 100H–170H
    /// (SDM §13.8.4 Figure 13-20, APM Table 16-2).
    fn isr(vector: u8) -> Reg {
        Reg(0x100 + 0x10 * (u32::from(vector) >> 5))
    }

    #[inline]
    fn read(self) -> u32 {
        match control_regs::apic_mode() {
            ApicMode::X2apic => cpu::rdmsr(self.msr()) as u32,
            ApicMode::Xapic => window().read_u32(u64::from(self.0)),
        }
    }

    #[inline]
    fn write(self, value: u32) {
        match control_regs::apic_mode() {
            // SAFETY: an architectural local-APIC register (the constants
            // above), written a word this module built from its field
            // encodings, so no reserved-bit `#GP` is possible.
            ApicMode::X2apic => unsafe { cpu::wrmsr(self.msr(), u64::from(value)) },
            ApicMode::Xapic => window().write_u32(u64::from(self.0), value),
        }
    }

    fn msr(self) -> u32 {
        0x800 + (self.0 >> 4)
    }
}

/// The xAPIC register page, a whole 4 KiB (SDM §13.4.1, APM §16.3.2), which
/// [`init`] maps uncacheable before anything reaches it: every access to it
/// is then serializing (SDM §13.12.3's note), so none owes a fence.
fn window() -> Mmio {
    Mmio::new(DirectMap::from_phys(control_regs::APIC_REGISTERS), 0x1000)
}

pub const TIMER_VECTOR: u8 = 0x20;

/// Where a device writes a message-signalled interrupt: the local APIC's
/// message window (SDM §13.11.1). The one spelling of it — the
/// compatibility format below, VT-d's remappable format and VT-d's own fault
/// event all start here.
pub const MSI_DOORBELL: u32 = 0xFEE0_0000;

/// `apic_id` as the eight bits an xAPIC ICR, a compatibility-format MSI and
/// an I/O APIC entry carry it in (SDM §13.6.1, §13.11.1), or the refusal:
/// `0xFF` there is the broadcast, not a CPU.
pub fn narrow_destination(apic_id: u32) -> Result<u8, &'static str> {
    u8::try_from(apic_id)
        .ok()
        .filter(|id| *id != 0xFF)
        .ok_or("the APIC id does not fit an 8-bit destination, where 0xFF is broadcast")
}

/// The compatibility-format message that raises `vector` on the CPU whose APIC
/// ID is `dest`: the destination in address bits 19:12, the vector in the data.
pub fn msi_message(dest: u32, vector: u8) -> Result<(u32, u32), &'static str> {
    Ok((MSI_DOORBELL | u32::from(narrow_destination(dest)?) << 12, vector as u32))
}

/// Calibrated LAPIC timer ticks per 10ms (computed on BSP, reused by APs),
/// which is one quantum: what the Ring 0 timer branch re-arms with.
static TIMER_TICKS: AtomicU32 = AtomicU32::new(0);
const _: () = assert!(kernel::sched::fair::QUANTUM_NS == 10_000_000);

/// The spurious-interrupt vector register, whole (SDM §13.9, APM §16.4.7): the
/// APIC enabled, the vector `arch::idt::spurious` gates, and every other bit
/// clear — EOI-broadcast suppression among them, so an EOI for a level line
/// reaches the I/O APIC and clears its Remote IRR.
const SVR: u32 = 1 << 8 | super::idt::spurious::SPURIOUS_VECTOR as u32;

/// Guards IPI sends before the BSP's APIC is enabled.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// This CPU's `IA32_APIC_BASE` as declared, then its SVR, read back.
fn enable(cpu_id: u32) -> ApicMode {
    let mode = control_regs::init_apic(cpu_id);
    // The BSP's alone: every AP reaches the same page through the kernel's tables.
    if cpu_id == 0 && mode == ApicMode::Xapic {
        crate::mm::paging::map_mmio(control_regs::APIC_REGISTERS, 0x1000, MmioPolicy::Uncacheable);
    }
    Reg::SVR.write(SVR);
    let held = Reg::SVR.read();
    assert!(held == SVR, "LAPIC: SVR reads {held:#x} after {SVR:#x} was written");
    mode
}

/// Enable the BSP's local APIC.
pub fn init() {
    let mode = enable(0);
    ENABLED.store(true, Ordering::Release);
    log!("LAPIC: {mode:?} enabled (ID {})", id());
}

/// Enable this AP's local APIC, in the mode the BSP's took.
pub fn init_ap() {
    enable(percpu::cpu_id());
}

/// This CPU's APIC id: all 32 bits in x2APIC, bits 31:24 in xAPIC (SDM
/// §13.4.6 Figure 13-6, APM §16.3.3, §16.12).
pub fn id() -> u32 {
    match control_regs::apic_mode() {
        ApicMode::X2apic => Reg::ID.read(),
        ApicMode::Xapic => Reg::ID.read() >> 24,
    }
}

/// Raise the interrupt `command` names — the ICR's low half — at the CPU
/// whose APIC id is `apic_id`, or at the destination its shorthand names
/// when `None`, after every store before it.
///
/// x2APIC: one write, behind a fence, since that write is not serializing
/// (SDM §13.12.3, APM §16.11.2) and a target could otherwise take the
/// interrupt before the store it announces. xAPIC: the destination half and
/// then the command half, whose write sends (SDM §13.6.1, APM §16.5), with
/// interrupts closed between them so a handler's own send cannot retarget
/// this one — an NMI handler's targeted send could, and the only one is the
/// boot actuators' nested-NMI staging. Delivery Status is not polled: APM
/// §16.5 lets the ICR be written again without it, and SDM §13.6.1 asks no
/// wait.
fn send(apic_id: Option<u32>, command: u32) {
    match control_regs::apic_mode() {
        ApicMode::X2apic => {
            let icr = u64::from(apic_id.unwrap_or(0)) << 32 | u64::from(command);
            cpu::wrmsr_fence();
            // SAFETY: the ICR, written a command this module encodes and a
            // 32-bit destination, which every x2APIC accepts.
            unsafe { cpu::wrmsr(Reg::ICR.msr(), icr) };
        }
        ApicMode::Xapic => {
            let _closed = super::IrqGuard::close();
            if let Some(apic_id) = apic_id {
                let dest = narrow_destination(apic_id)
                    .unwrap_or_else(|why| panic!("LAPIC: no IPI can reach APIC id {apic_id:#x}: {why}"));
                Reg::ICR_HIGH.write(u32::from(dest) << 24);
            }
            Reg::ICR.write(command);
        }
    }
}

/// Send INIT IPI to the specified APIC ID.
pub fn send_init(apic_id: u32) {
    // 0x4500 = delivery INIT, level assert.
    send(Some(apic_id), 0x4500);
}

/// Send Startup IPI (SIPI) with the given vector (trampoline page number).
pub fn send_sipi(apic_id: u32, vector: u8) {
    send(Some(apic_id), 0x4600 | u32::from(vector));
}

/// Send EOI.
#[inline]
pub fn eoi() {
    Reg::EOI.write(0);
}

/// Whether `vector` is in service on this CPU.
pub fn in_service(vector: u8) -> bool {
    let word = Reg::isr(vector).read();
    (word >> (vector & 31)) & 1 != 0
}

/// The highest vector in service — the one being handled, since the LAPIC only
/// delivers above the ISR top (SDM §13.8.4). `None` outside a handler.
pub fn in_service_highest() -> Option<u8> {
    for word_index in (0..8u32).rev() {
        let word = Reg::isr((word_index * 32) as u8).read();
        if word != 0 {
            return Some((word_index * 32 + (31 - word.leading_zeros())) as u8);
        }
    }
    None
}

/// Send an IPI to this CPU (self shorthand).
#[cfg(feature = "boot-actuators")]
pub fn send_self(vector: u8) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // Destination shorthand = self (0b01 << 18), fixed delivery, level assert.
    send(None, 0x0004_4000 | u32::from(vector));
}

fn ipi_all_excluding_self(vector: u8) {
    // destination shorthand = all-excluding-self (0b11 << 18), fixed delivery
    send(None, 0x000C_0000 | u32::from(vector));
}

/// Ask every other CPU to flush its TLB.
pub(super) fn tlb_ipi() {
    if ENABLED.load(Ordering::Relaxed) {
        ipi_all_excluding_self(0xFE);
    }
}

/// Send the kick IPI to one CPU, waking it if halted.
// Targeted, not broadcast: a broadcast kick would preempt every sibling per wake and cannot scale.
pub fn kick_cpu(cpu_id: u32) {
    if !ENABLED.load(Ordering::Relaxed) { return; }
    let apic_id = crate::smp::hardware_id(cpu_id);
    send(Some(apic_id), 0x4000 | u32::from(super::idt::KICK_VECTOR));
}

// Kicked, and not left to arrive on their own: a CPU halted in the idle path has stopped its own timer, so nothing else brings it to the next scheduler pass.
pub fn kick_all_but_self() {
    let me = percpu::cpu_id();
    for cpu in 0..crate::smp::cpu_count() {
        if cpu != me {
            kick_cpu(cpu);
        }
    }
}

/// Send a non-maskable interrupt to one CPU: a test kernel's staged NMI, since IF cannot mask one.
// Test kernels only: an NMI can land inside any critical section, which this kernel cannot make NMI-safe.
#[cfg(feature = "boot-actuators")]
pub fn send_nmi(cpu_id: u32) {
    if !ENABLED.load(Ordering::Relaxed) { return; }
    let apic_id = crate::smp::hardware_id(cpu_id);
    send(Some(apic_id), 0x4400);
}

/// Point this CPU's performance-counter LVT entry at NMI delivery, unmasked —
/// the one interrupt a CPU that has cleared `IF` still takes, and so the only
/// way `crate::hardlockup` can sample one.
///
/// **Written again after every delivery, not once at arm.** SDM §13.5.1, APM
/// §16.4.3: the local APIC sets this entry's mask flag when it handles a
/// performance-monitoring interrupt, and only software clears it, so a handler
/// that does not write this gets exactly one NMI for the machine's life.
///
/// Declared whole: delivery mode 100b (NMI) in bits 10:8, mask clear, and a
/// vector field the CPU ignores under that delivery mode.
pub fn arm_perf_nmi() {
    if !ENABLED.load(Ordering::Relaxed) { return; }
    Reg::LVT_PMC.write(0b100 << 8);
}

/// Send every other CPU the halt IPI, where the machine has released any: before
/// that no sibling has been sent its `SIPI`, so there are only CPUs still waiting
/// for one rather than CPUs that need halting.
pub fn stop_other_cpus() {
    if ENABLED.load(Ordering::Relaxed) && crate::smp::is_ready() {
        send(None, 0x000C_0000 | 0xFD);
    }
}

/// Calibrate the LAPIC timer on the BSP (requires HPET); does not start it.
pub fn init_timer() {
    // Divide by 1 for maximum resolution.
    Reg::TIMER_DIVIDE.write(0b1011);

    // Masked one-shot mode for calibration.
    Reg::LVT_TIMER.write(1 << 16);
    Reg::TIMER_INIT.write(0xFFFF_FFFF);

    const CALIBRATION: Delay = Delay::to_measure(
        Duration::from_millis(10),
        "LAPIC ticks counted against the monotonic clock, and the tick figure is reported per 10ms",
    );
    let start = crate::clock::nanos_since_boot();
    while crate::clock::nanos_since_boot() - start < CALIBRATION.nanos() {}
    let elapsed = crate::clock::nanos_since_boot() - start;

    let remaining = Reg::TIMER_CURRENT.read();
    let ticks_elapsed = 0xFFFF_FFFFu32.wrapping_sub(remaining);
    let ticks_10ms = (ticks_elapsed as u64 * 10_000_000 / elapsed) as u32;

    Reg::TIMER_INIT.write(0);
    TIMER_TICKS.store(ticks_10ms, Ordering::Release);
    // Fallback for any Ring 0 fire before the scheduler arms its first quantum.
    percpu::set_last_armed_ticks(OneShot::ticks(ticks_10ms as u64).0);
    // The implied hertz is the machine's third timebase, and it is *not* a
    // check on the TSC: this count was measured against the TSC-derived clock,
    // so agreement between them is arithmetic. What it is is a number of the
    // part's own — the bus clock the LAPIC counts — stable across boots of one
    // machine, so a profile can hold a ceiling against a boot that moved it.
    log!("LAPIC timer: {} ticks/10ms, so {}Hz", ticks_10ms, ticks_10ms as u64 * 100);
}

// The floor is enforced once here, not at each of the three call sites.
struct OneShot(u32);

impl OneShot {
    fn ticks(ticks: u64) -> Self {
        let per_10ms = TIMER_TICKS.load(Ordering::Relaxed) as u64;
        let floor = (MIN_ONE_SHOT.nanos() * per_10ms / 10_000_000).max(1);
        // Zero means stop_timer, not a valid count — `min` alone would let a small calibration write it.
        Self(ticks.clamp(floor, u32::MAX as u64) as u32)
    }

    fn after(nanos: u64) -> Self {
        let per_10ms = TIMER_TICKS.load(Ordering::Relaxed) as u128;
        Self::ticks((nanos as u128 * per_10ms / 10_000_000) as u64)
    }

    fn arm(self) {
        Reg::TIMER_DIVIDE.write(0b1011);
        // LVT resets masked; an AP may reach here before this register was ever written.
        Reg::LVT_TIMER.write(u32::from(TIMER_VECTOR));
        percpu::set_last_armed_ticks(self.0);
        Reg::TIMER_INIT.write(self.0);
    }
}

/// Arm a one-shot timer to fire after `nanos` nanoseconds, or after [`MIN_ONE_SHOT`] if that is longer.
pub fn arm_one_shot(nanos: u64) {
    OneShot::after(nanos).arm();
    crate::trace::trace(crate::trace::Kind::TimerArm, nanos as u32);
}

/// Shorten this CPU's armed interval to at most `nanos`, arming it if stopped.
// Traces nothing, unlike arm_one_shot: no scheduler deadline is being set here, and a TimerArm record would misreport one.
pub fn arm_within(nanos: u64) {
    let want = OneShot::after(nanos);
    // Zero here means stop_timer, not an imminent expiry — a running count never reaches zero on its own.
    let remaining = Reg::TIMER_CURRENT.read();
    let ticks = if remaining == 0 { want.0 } else { want.0.min(remaining) };
    OneShot::ticks(ticks as u64).arm();
}

/// What the timer was last armed with: `TIMER_INIT`, a count of ticks.
#[cfg(feature = "test-actuators")]
pub fn comparator() -> u64 {
    u64::from(Reg::TIMER_INIT.read())
}

/// Whether a Ring 0 fire since [`comparator`] read `_armed` re-armed one quantum.
#[cfg(feature = "test-actuators")]
pub fn rearmed_a_quantum(_armed: u64) -> bool {
    Reg::TIMER_INIT.read() == TIMER_TICKS.load(Ordering::Relaxed)
}

/// The Ring 3 fire's re-arm, before its Rust half runs: what this CPU last
/// armed, again, so the timer survives a handler that panics first.
pub(crate) extern "sysv64" fn rearm_last() {
    Reg::TIMER_INIT.write(percpu::last_armed_ticks());
}

/// The Ring 0 fire's local-APIC work, called from its stub with interrupts
/// closed: the EOI, then one quantum on, or a stopped timer left stopped.
pub(crate) extern "sysv64" fn ring0_fire() {
    eoi();
    let stopped = percpu::last_armed_ticks() == 0;
    Reg::TIMER_INIT.write(if stopped { 0 } else { TIMER_TICKS.load(Ordering::Relaxed) });
}

/// Stop the timer. No more interrupts until re-armed.
pub fn stop_timer() {
    percpu::set_last_armed_ticks(0);
    Reg::TIMER_INIT.write(0);
    crate::trace::trace(crate::trace::Kind::TimerStop, 0);
}
