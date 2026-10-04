//! Which of this CPU's model-specific counters it reads: decided on the CPU
//! itself at bring-up, by `toyos_cpuvuln::counters` over its own CPUID, and
//! the power envelope where `control_regs` declared it.

use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use toyos_cpuvuln::CounterFacts;

use super::{cpu, percpu};
use crate::counters::Hardware;
use crate::scheduler::MAX_CPUS;

const MSR_SMI_COUNT: u32 = 0x34;
const IA32_MPERF: u32 = 0xE7;
const IA32_APERF: u32 = 0xE8;

const SMI: u8 = 1 << 0;
const APERF_MPERF: u8 = 1 << 1;

/// Each CPU's verdict, written by that CPU before it reads a counter.
static ADMITTED: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];

pub fn bring_up() {
    let max_leaf = cpu::cpuid(0, 0).0;
    let (signature, _, cpuid_1_ecx, _) = cpu::cpuid(1, 0);
    // As `init_scattered_cpuid_features` reads it: a level past the range's own is no level.
    let cpuid_6_ecx = if (6..=0xFFFF).contains(&max_leaf) { cpu::cpuid(6, 0).2 } else { 0 };
    let verdict = toyos_cpuvuln::counters(&CounterFacts {
        vendor: cpu::vendor(),
        signature,
        cpuid_1_ecx,
        cpuid_6_ecx,
    });
    let bits = if verdict.smi { SMI } else { 0 } | if verdict.aperf_mperf { APERF_MPERF } else { 0 };
    ADMITTED[percpu::cpu_id() as usize].store(bits, Relaxed);
}

/// This CPU's admitted counters.
pub fn read() -> Hardware {
    let admitted = ADMITTED[percpu::cpu_id() as usize].load(Relaxed);
    let msr = |bit: u8, msr: u32| (admitted & bit != 0).then(|| cpu::rdmsr(msr));
    Hardware {
        // Bits 63:32 are reserved (SDM Vol. 4, MSR 34H).
        smi: msr(SMI, MSR_SMI_COUNT).map(|count| count & 0xFFFF_FFFF),
        aperf: msr(APERF_MPERF, IA32_APERF),
        mperf: msr(APERF_MPERF, IA32_MPERF),
        envelope: super::control_regs::envelope(),
    }
}
