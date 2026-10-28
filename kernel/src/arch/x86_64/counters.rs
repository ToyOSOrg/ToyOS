//! Which of this CPU's model-specific counters it reads: decided on the CPU
//! itself at bring-up, by `kernel::cpu::counters` over its own CPUID, and
//! the power envelope where `control_regs` declared it.

use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use kernel::cpu::CounterFacts;

use kernel::cpu::Rapl;

use super::cpu;
use crate::counters::{Firmware, Hardware, Package};

const MSR_SMI_COUNT: u32 = 0x34;
const MSR_CORE_C6_RESIDENCY: u32 = 0x3FD;
const MSR_CORE_C7_RESIDENCY: u32 = 0x3FE;
const IA32_MPERF: u32 = 0xE7;
const IA32_APERF: u32 = 0xE8;
// SDM Vol. 4 Table 2-20.
const MSR_RAPL_POWER_UNIT: u32 = 0x606;
const MSR_PKG_ENERGY_STATUS: u32 = 0x611;
const MSR_PKG_C2_RESIDENCY: u32 = 0x60D;
const MSR_PKG_C3_RESIDENCY: u32 = 0x3F8;
const MSR_PKG_C6_RESIDENCY: u32 = 0x3F9;
const MSR_PKG_C7_RESIDENCY: u32 = 0x3FA;
// PPR 55922 `RAPL_PWR_UNIT`, `CORE_ENERGY_STAT` and `PKG_ENERGY_STAT`.
const AMD_RAPL_PWR_UNIT: u32 = 0xC001_0299;
const AMD_CORE_ENERGY_STAT: u32 = 0xC001_029A;
const AMD_PKG_ENERGY_STAT: u32 = 0xC001_029B;

const SMI: u8 = 1 << 0;
const APERF_MPERF: u8 = 1 << 1;
const CORE_RESIDENCY: u8 = 1 << 2;
const INTEL_RAPL: u8 = 1 << 3;
const AMD_RAPL: u8 = 1 << 4;
const PACKAGE_RESIDENCY: u8 = 1 << 5;
const CORE_ENERGY: u8 = 1 << 6;
/// Bits 31:0 of every energy register: bits 63:32 are reserved (SDM Vol. 3B
/// Figure 17-37; PPR 55922).
const ENERGY: u64 = 0xFFFF_FFFF;

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
    let cpuid_8000_0007_edx =
        if cpu::cpuid(0x8000_0000, 0).0 >= 0x8000_0007 { cpu::cpuid(0x8000_0007, 0).3 } else { 0 };
    let verdict = kernel::cpu::counters(&CounterFacts {
        vendor: cpu::vendor(),
        signature,
        cpuid_1_ecx,
        cpuid_6_ecx,
        cpuid_8000_0007_edx,
        reads_package: super::percpu::cpu_id() == 0,
    });
    let bits = if verdict.smi { SMI } else { 0 }
        | if verdict.aperf_mperf { APERF_MPERF } else { 0 }
        | if verdict.core_residency { CORE_RESIDENCY } else { 0 }
        | match verdict.package_energy {
            Some(Rapl::Intel) => INTEL_RAPL,
            Some(Rapl::Amd) => AMD_RAPL,
            None => 0,
        }
        | if verdict.package_residency { PACKAGE_RESIDENCY } else { 0 }
        | if verdict.core_energy { CORE_ENERGY } else { 0 };
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
        package: msr(INTEL_RAPL, MSR_PKG_ENERGY_STATUS)
            .map(|energy| Package { energy: energy & ENERGY, power_unit: cpu::rdmsr(MSR_RAPL_POWER_UNIT) })
            .or_else(|| {
                msr(AMD_RAPL, AMD_PKG_ENERGY_STAT)
                    .map(|energy| Package { energy: energy & ENERGY, power_unit: cpu::rdmsr(AMD_RAPL_PWR_UNIT) })
            }),
        package_residency: (admitted & PACKAGE_RESIDENCY != 0).then(|| {
            [MSR_PKG_C2_RESIDENCY, MSR_PKG_C3_RESIDENCY, MSR_PKG_C6_RESIDENCY, MSR_PKG_C7_RESIDENCY].map(cpu::rdmsr)
        }),
        core_energy: msr(CORE_ENERGY, AMD_CORE_ENERGY_STAT).map(|energy| energy & ENERGY),
    }
}
