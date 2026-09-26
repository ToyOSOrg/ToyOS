//! The performance-monitoring counter that samples a CPU with an NMI: fixed
//! counter 2, counting the reference clock, overflowing into the local APIC's
//! performance-counter LVT (SDM Vol. 3B §20.2.2 for the counters, Vol. 3A
//! §12.5.1 for the LVT, Vol. 2A CPUID leaf 0AH for whether there are any).
//! `crate::hardlockup` decides what a sample means; this is how one arrives.
//!
//! Every path here runs from an NMI or on the CPU it arms: no lock, no log.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{apic, cpu};

/// The architectural performance-monitoring MSRs this file writes, SDM Vol. 3B
/// §20.2.2 and Vol. 4 Table 2-2. Nothing else in this kernel programs the PMU,
/// which is why each control register below is declared whole and written
/// whole rather than read, modified and written back.
const IA32_FIXED_CTR2: u32 = 0x30B;
const IA32_FIXED_CTR_CTRL: u32 = 0x38D;
const IA32_PERF_GLOBAL_STATUS: u32 = 0x38E;
const IA32_PERF_GLOBAL_CTRL: u32 = 0x38F;
const IA32_PERF_GLOBAL_OVF_CTRL: u32 = 0x390;

/// Fixed counter 2's nibble of `IA32_FIXED_CTR_CTRL` is bits 11:8: enable in
/// ring 0 (bit 8) and ring 3 (bit 9), no AnyThread (bit 10), and PMI on
/// overflow (bit 11). Counting in both rings, because a CPU that stops taking
/// interrupts in either is the same defect.
const FIXED_CTR2_ARMED: u64 = 0b1011 << 8;

/// The fixed counters live in the high half of `IA32_PERF_GLOBAL_CTRL` and of
/// the status and overflow-clear registers beside it, so counter 2 is bit 34.
const GLOBAL_FIXED_CTR2: u64 = 1 << 34;

/// The counter's width as CPUID states it, as a mask: bits above it may not be
/// written back.
static WIDTH_MASK: AtomicU64 = AtomicU64::new(0);

/// The counter's width, or `None` on a CPU with no architectural performance
/// monitoring — which is every QEMU TCG guest.
///
/// SDM Vol. 2A, CPUID leaf 0AH: EAX[7:0] is the version, and version 2 is where
/// the fixed-function counters and `IA32_PERF_GLOBAL_CTRL` appear; EDX[4:0] is
/// how many fixed counters there are and EDX[12:5] how wide they are. Fixed
/// counter 2 needs three of them.
fn architectural_pmu() -> Option<u32> {
    if cpu::cpuid(0, 0).0 < 0x0A {
        return None;
    }
    let (eax, _, _, edx) = cpu::cpuid(0x0A, 0);
    let version = eax & 0xff;
    let counters = edx & 0x1f;
    let width = (edx >> 5) & 0xff;
    if version < 2 || counters < 3 || width == 0 || width > 64 {
        return None;
    }
    Some(width)
}

const fn mask_of(width: u32) -> u64 {
    match width >= 64 {
        true => u64::MAX,
        false => (1u64 << width) - 1,
    }
}

/// Start this CPU's counter overflowing into an NMI every `period` reference
/// cycles — which are TSC ticks — or answer `false` for a CPU that has none.
pub fn arm(period: u64) -> bool {
    let Some(width) = architectural_pmu() else { return false };
    WIDTH_MASK.store(mask_of(width), Relaxed);
    // Stopped, then set up, then started: a counter enabled while its control
    // register is half written can overflow into an LVT that is not armed yet.
    write_msr(IA32_PERF_GLOBAL_CTRL, 0);
    write_msr(IA32_FIXED_CTR_CTRL, FIXED_CTR2_ARMED);
    write_msr(IA32_PERF_GLOBAL_OVF_CTRL, GLOBAL_FIXED_CTR2);
    reload(period);
    apic::arm_perf_nmi();
    write_msr(IA32_PERF_GLOBAL_CTRL, GLOBAL_FIXED_CTR2);
    true
}

/// Whether this CPU's counter is the reason this NMI arrived:
/// `IA32_PERF_GLOBAL_STATUS` bit 34 is its overflow.
pub fn overflowed() -> bool {
    cpu::rdmsr(IA32_PERF_GLOBAL_STATUS) & GLOBAL_FIXED_CTR2 != 0
}

/// After an overflow's sample: clear it, reload, and unmask the LVT hardware
/// masked on delivery.
pub fn rearm(period: u64) {
    write_msr(IA32_PERF_GLOBAL_OVF_CTRL, GLOBAL_FIXED_CTR2);
    reload(period);
    apic::arm_perf_nmi();
}

/// Set the counter one period below its own overflow.
fn reload(period: u64) {
    let mask = WIDTH_MASK.load(Relaxed);
    // Masked to the width CPUID stated: a fixed counter refuses a write of the
    // bits above it, and the negative count is what makes the overflow land a
    // period from here.
    write_msr(IA32_FIXED_CTR2, 0u64.wrapping_sub(period) & mask);
}

fn write_msr(msr: u32, value: u64) {
    // SAFETY: every MSR here is an architectural performance-monitoring counter
    // or its control register, enumerated by CPUID leaf 0AH before this file
    // writes any of them, and each value is that register's own field encoding.
    unsafe { cpu::wrmsr(msr, value) };
}
