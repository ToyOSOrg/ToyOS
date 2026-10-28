//! Which of this CPU's model-specific counters it reads: decided on the CPU
//! itself at bring-up, by `kernel::cpu::counters` over its own CPUID, and
//! the power envelope where `control_regs` declared it.

use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use kernel::cpu::CounterFacts;

use super::cpu;
use crate::counters::{Firmware, Hardware};

const MSR_SMI_COUNT: u32 = 0x34;
const MSR_CORE_C6_RESIDENCY: u32 = 0x3FD;
const MSR_CORE_C7_RESIDENCY: u32 = 0x3FE;
const IA32_MPERF: u32 = 0xE7;
const IA32_APERF: u32 = 0xE8;

const SMI: u8 = 1 << 0;
const APERF_MPERF: u8 = 1 << 1;
const CORE_RESIDENCY: u8 = 1 << 2;

/// A CPU's verdict, written by that CPU before it reads a counter.
pub struct Local {
    admitted: AtomicU8,
}

impl Local {
    pub fn new() -> Self {
        Self { admitted: AtomicU8::new(0) }
    }
}

fn admitted() -> &'static AtomicU8 {
    &crate::cpu_local::here().arch.counters.admitted
}

pub fn bring_up() {
    let (signature, _, cpuid_1_ecx, _) = cpu::cpuid(1, 0);
    let (_, cpuid_6_ecx) = cpu::leaf_6();
    let verdict = kernel::cpu::counters(&CounterFacts {
        vendor: cpu::vendor(),
        signature,
        cpuid_1_ecx,
        cpuid_6_ecx,
    });
    let bits = if verdict.smi { SMI } else { 0 }
        | if verdict.aperf_mperf { APERF_MPERF } else { 0 }
        | if verdict.core_residency { CORE_RESIDENCY } else { 0 };
    admitted().store(bits, Relaxed);
}

/// This CPU's admitted counters.
pub fn read() -> Hardware {
    let admitted = admitted().load(Relaxed);
    let msr = |bit: u8, msr: u32| (admitted & bit != 0).then(|| cpu::rdmsr(msr));
    Hardware {
        // Bits 63:32 are reserved (SDM Vol. 4, MSR 34H).
        smi: msr(SMI, MSR_SMI_COUNT).map(|count| count & 0xFFFF_FFFF),
        aperf: msr(APERF_MPERF, IA32_APERF),
        mperf: msr(APERF_MPERF, IA32_MPERF),
        envelope: super::control_regs::envelope(),
        firmware: super::smi_cmd::counted().map(|(calls, nanos)| Firmware { calls, nanos }),
        core_c6: msr(CORE_RESIDENCY, MSR_CORE_C6_RESIDENCY),
        core_c7: msr(CORE_RESIDENCY, MSR_CORE_C7_RESIDENCY),
    }
}
