use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{cpu, percpu};
use crate::log;
use crate::time::{Budget, Delay, Duration, Floor};

/// The local APIC registers and MSRs this file may name.
// Every variant is an architectural local-APIC register touching no memory or control transfer, so none can make `Reg::write`'s unsafe wrmsr unsound.
// Addresses are x2APIC's: 0x800 plus the xAPIC MMIO offset shifted right four; ApicBase is the one exception, the MSR that turns x2APIC on.
#[derive(Clone, Copy)]
#[repr(u32)]
enum Reg {
    ApicBase = 0x1B,
    Id = 0x802,
    Eoi = 0x80B,
    Svr = 0x80F,
    /// First of the eight in-service words (0x810..=0x817); see `in_service`.
    Isr0 = 0x810,
    Icr = 0x830,
    LvtTimer = 0x832,
    TimerInit = 0x838,
    TimerCurrent = 0x839,
    TimerDivide = 0x83E,
}

impl Reg {
    #[inline]
    fn read(self) -> u64 {
        cpu::rdmsr(self as u32)
    }

    #[inline]
    fn write(self, value: u64) {
        // SAFETY: every value written here is a local-APIC word built in this module from architectural field encodings, so no reserved-bit #GP is possible.
        unsafe { cpu::wrmsr(self as u32, value) };
    }
}

pub const TIMER_VECTOR: u8 = 0x20;

/// Calibrated LAPIC timer ticks per 10ms (computed on BSP, reused by APs).
static TIMER_TICKS: AtomicU32 = AtomicU32::new(0);

/// Guards IPI sends before the APIC is enabled.
static X2APIC_ENABLED: AtomicBool = AtomicBool::new(false);

fn enable_x2apic() {
    let mut base = Reg::ApicBase.read();
    base |= (1 << 11) | (1 << 10);
    Reg::ApicBase.write(base);

    // The low byte must match arch::idt::spurious's gate vector.
    let svr = Reg::Svr.read();
    Reg::Svr.write(svr | (1 << 8) | super::idt::spurious::SPURIOUS_VECTOR as u64);
}

/// Initialize the BSP's Local APIC in x2APIC mode.
pub fn init() {
    enable_x2apic();
    X2APIC_ENABLED.store(true, Ordering::Release);
    log!("LAPIC: x2APIC enabled (ID {})", id());
}

/// Enable the AP's local APIC in x2APIC mode.
pub fn init_ap() {
    enable_x2apic();
}

pub fn id() -> u32 {
    Reg::Id.read() as u32
}

/// Send INIT IPI to the specified APIC ID.
pub fn send_init(apic_id: u32) {
    // ICR write: destination in the high 32 bits, 0x4500 = delivery INIT, level assert.
    Reg::Icr.write(((apic_id as u64) << 32) | 0x4500);
}

/// Send Startup IPI (SIPI) with the given vector (trampoline page number).
pub fn send_sipi(apic_id: u32, vector: u8) {
    Reg::Icr.write(((apic_id as u64) << 32) | 0x4600 | vector as u64);
}

/// Send EOI.
#[inline]
pub fn eoi() {
    Reg::Eoi.write(0);
}

/// Whether `vector` is in service on this CPU.
pub fn in_service(vector: u8) -> bool {
    // ISR is eight 32-bit words (SDM Vol. 3A §12.8.4): word = vector >> 5, bit = vector & 31.
    let word = cpu::rdmsr(Reg::Isr0 as u32 + (vector as u32 >> 5));
    (word >> (vector & 31)) & 1 != 0
}

/// Send an IPI to this CPU (self shorthand).
#[cfg(feature = "boot-actuators")]
pub fn send_self(vector: u8) {
    if !X2APIC_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // Destination shorthand = self (0b01 << 18), fixed delivery, level assert.
    Reg::Icr.write(0x0004_4000 | vector as u64);
}

fn ipi_all_excluding_self(vector: u8) {
    // destination shorthand = all-excluding-self (0b11 << 18), fixed delivery
    Reg::Icr.write(0x000C_0000 | vector as u64);
}

/// Ask every other CPU to flush its TLB.
pub(super) fn tlb_ipi() {
    if X2APIC_ENABLED.load(Ordering::Relaxed) {
        ipi_all_excluding_self(0xFE);
    }
}

/// Send the timer-vector IPI to one CPU, waking it if halted.
// Targeted, not broadcast: a broadcast kick would preempt every sibling per wake and cannot scale.
pub fn kick_cpu(cpu_id: u32) {
    if !X2APIC_ENABLED.load(Ordering::Relaxed) { return; }
    let apic_id = crate::arch::smp::apic_id_for(cpu_id);
    Reg::Icr.write(((apic_id as u64) << 32) | 0x4000 | TIMER_VECTOR as u64);
}

/// Send a non-maskable interrupt to one CPU — for a CPU that failed to answer `kick_cpu`, since IF cannot mask NMI.
// Diagnostic only: an NMI can land inside any critical section, which this kernel cannot make NMI-safe.
pub fn send_nmi(cpu_id: u32) {
    if !X2APIC_ENABLED.load(Ordering::Relaxed) { return; }
    let apic_id = crate::arch::smp::apic_id_for(cpu_id);
    Reg::Icr.write(((apic_id as u64) << 32) | 0x4400);
}

// Time /bin/logd gets to durably write the panic report before halt; a Budget (not Bound) because expiry degrades gracefully instead of panicking.
const LOG_FILE_DRAIN: Budget = Budget::of(
    Duration::from_millis(500),
    "the report reaches the panel and not /log",
);

// Read by tests/toyos.rs — keep in sync or its drift check fails.
const LOG_DRAIN_EXPIRED: &str = "the report did not reach /log";

/// Whether `/log` still owes this boot the report.
// True only before durable_ns passes `want` — logd publishes it after fsync returns, never before.
fn owed(want: u64) -> bool {
    crate::log::user::durable_ns() < want
}

/// Give `/bin/logd` a chance to put this report on the stick before the machine stops.
// The panic path never writes /log directly — every lock a write needs may already be held by the panicking thread itself.
fn wait_for_log_file() {
    // Skip when serial exists: panic_flush already got the report off the box, and waiting here would only delay the pager.
    if crate::drivers::serial::has_console() {
        return;
    }
    // Sampled once — a sibling still logging on its way down must not be able to push this deadline out indefinitely.
    let want = crate::log::read::newest_committed_at_ns();
    if !owed(want) {
        return;
    }
    // Wake siblings first: one may be halted in `sti; hlt` with no timer armed to wake it otherwise.
    let cpus = crate::arch::smp::cpu_count();
    let me = percpu::cpu_id();
    for cpu in 0..cpus {
        if cpu != me {
            kick_cpu(cpu);
        }
    }
    let deadline = crate::clock::now() + LOG_FILE_DRAIN.duration();
    while owed(want) {
        if crate::clock::now() >= deadline {
            // /log has failed to answer, so fold this into the still-unpainted panel snapshot — the panel is the only channel left.
            crate::log!(
                "panic: {LOG_DRAIN_EXPIRED} in {}ns; the panel is the only copy",
                LOG_FILE_DRAIN.nanos()
            );
            crate::drivers::panic_console::refresh_capture();
            return;
        }
        core::hint::spin_loop();
    }
}

/// Halt all CPUs: send the halt IPI, flush pending log output, then halt self.
// panic_flush bypasses the log-ring and serial locks — after the halt IPI a wedged holder never releases them, so taking them normally could deadlock.
pub fn halt_all_cpus() -> ! {
    wait_for_log_file();
    if X2APIC_ENABLED.load(Ordering::Relaxed) {
        Reg::Icr.write(0x000C_0000 | 0xFD);
    }
    // Render before the flush: it can't fail the proven serial channel, and a serial line then proves the paint already finished.
    let painted = crate::drivers::panic_console::render();
    // SAFETY: sound only once nothing else will run — the halt IPI is already out.
    unsafe { crate::drivers::serial::panic_flush(); }
    // Must follow the flush — it's the deepest stack this path reaches. No-op off IST1.
    percpu::ist1_report();
    // page_forever runs strictly after the flush: it is an unbounded loop and may only run once the serial report is out.
    // Only the CPU that painted enters the pager; the rest halt directly below.
    if painted {
        crate::drivers::panic_console::page_forever();
    }
    super::cpu::halt();
}

/// Calibrate the LAPIC timer on the BSP (requires HPET); does not start it.
pub fn init_timer() {
    // Divide by 1 for maximum resolution.
    Reg::TimerDivide.write(0b1011);

    // Masked one-shot mode for calibration.
    Reg::LvtTimer.write(1 << 16);
    Reg::TimerInit.write(0xFFFF_FFFF);

    const CALIBRATION: Delay = Delay::to_measure(
        Duration::from_millis(10),
        "LAPIC ticks counted against the monotonic clock, and the tick figure is reported per 10ms",
    );
    let start = crate::clock::nanos_since_boot();
    while crate::clock::nanos_since_boot() - start < CALIBRATION.nanos() {}
    let elapsed = crate::clock::nanos_since_boot() - start;

    let remaining = Reg::TimerCurrent.read() as u32;
    let ticks_elapsed = 0xFFFF_FFFFu32.wrapping_sub(remaining);
    let ticks_10ms = (ticks_elapsed as u64 * 10_000_000 / elapsed) as u32;

    Reg::TimerInit.write(0);
    TIMER_TICKS.store(ticks_10ms, Ordering::Release);
    // Fallback for any Ring 0 fire before the scheduler arms its first quantum.
    percpu::set_last_armed_ticks(OneShot::ticks(ticks_10ms as u64).0);
    log!("LAPIC timer: {} ticks/10ms", ticks_10ms);
}

// Floor on every arm: a count that expires before the interrupt it schedules retires cannot outlast itself and livelocks the CPU forever.
const MIN_ONE_SHOT: Floor = Floor::policy(
    Duration::from_micros(10),
    "above an interrupt entry and iretq, a thousandth of QUANTUM_NS",
);

// The only path to Reg::TimerInit / last_armed_ticks — the floor is enforced once here, not at each of the three call sites.
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
        Reg::TimerDivide.write(0b1011);
        // LVT resets masked; an AP may reach here before this register was ever written.
        Reg::LvtTimer.write(TIMER_VECTOR as u64);
        percpu::set_last_armed_ticks(self.0);
        Reg::TimerInit.write(self.0 as u64);
    }
}

/// Arm a one-shot timer to fire after `nanos` nanoseconds, or after [`MIN_ONE_SHOT`] if that is longer.
pub fn arm_one_shot(nanos: u64) {
    OneShot::after(nanos).arm();
    crate::trace::trace(crate::trace::Kind::TimerArm, nanos as u32);
}

/// Shorten this CPU's armed interval to at most `nanos`, arming it if stopped.
// Traces nothing, unlike arm_one_shot: no scheduler deadline is being set here, and a TimerArm record would misreport one.
#[cfg(feature = "boot-actuators")]
pub fn arm_within(nanos: u64) {
    let want = OneShot::after(nanos);
    // Zero here means stop_timer, not an imminent expiry — a running count never reaches zero on its own.
    let remaining = Reg::TimerCurrent.read() as u32;
    let ticks = if remaining == 0 { want.0 } else { want.0.min(remaining) };
    OneShot::ticks(ticks as u64).arm();
}

/// Stop the timer. No more interrupts until re-armed.
pub fn stop_timer() {
    percpu::set_last_armed_ticks(0);
    Reg::TimerInit.write(0);
    crate::trace::trace(crate::trace::Kind::TimerStop, 0);
}
