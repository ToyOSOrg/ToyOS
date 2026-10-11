//! Which of this CPU's model-specific counters it reads: decided on the CPU
//! itself at bring-up, by `toyos_cpuvuln::counters` over its own CPUID, and
//! the power envelope where `control_regs` declared it.

use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use toyos_cpuvuln::CounterFacts;

use super::{cpu, percpu};
use crate::counters::{Firmware, Hardware};
use crate::scheduler::MAX_CPUS;

const MSR_SMI_COUNT: u32 = 0x34;
const IA32_MPERF: u32 = 0xE7;
const IA32_APERF: u32 = 0xE8;

const SMI: u8 = 1 << 0;
const APERF_MPERF: u8 = 1 << 1;
/// MEASUREMENT ONLY: fixed counter 0 enabled here, and RAPL read.
const INSTRUCTIONS: u8 = 1 << 2;
const RAPL: u8 = 1 << 3;

/// MEASUREMENT ONLY, SDM Vol. 3B §20.2.2 and Vol. 4 §2.17.
const IA32_FIXED_CTR0: u32 = 0x309;
const IA32_FIXED_CTR_CTRL: u32 = 0x38D;
const IA32_PERF_GLOBAL_CTRL: u32 = 0x38F;
const MSR_RAPL_POWER_UNIT: u32 = 0x606;
const MSR_PKG_ENERGY_STATUS: u32 = 0x611;

/// MEASUREMENT ONLY: each CPU's last raw energy read and the microjoules
/// carried past every wrap before it, read and written on that CPU alone.
static ENERGY: [core::sync::atomic::AtomicU64; MAX_CPUS] = [const { core::sync::atomic::AtomicU64::new(0) }; MAX_CPUS];
static ENERGY_WRAPS: [core::sync::atomic::AtomicU64; MAX_CPUS] = [const { core::sync::atomic::AtomicU64::new(0) }; MAX_CPUS];
static ENERGY_UNIT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Each CPU's verdict, written by that CPU before it reads a counter.
static ADMITTED: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];

pub fn bring_up() {
    let (signature, _, cpuid_1_ecx, _) = cpu::cpuid(1, 0);
    let (_, cpuid_6_ecx) = cpu::leaf_6();
    let verdict = toyos_cpuvuln::counters(&CounterFacts {
        vendor: cpu::vendor(),
        signature,
        cpuid_1_ecx,
        cpuid_6_ecx,
    });
    let mut bits = if verdict.smi { SMI } else { 0 } | if verdict.aperf_mperf { APERF_MPERF } else { 0 };
    // MEASUREMENT ONLY: an Intel CPU with architectural performance monitoring
    // version 2 or later and a fixed counter counts its instructions, both
    // rings, and reads its package's RAPL energy.
    let intel = matches!(cpu::vendor(), toyos_cpuvuln::Vendor::Intel);
    let pmu = cpu::cpuid(0, 0).0 >= 0x0A && {
        let (eax, _, _, edx) = cpu::cpuid(0x0A, 0);
        eax & 0xff >= 2 && edx & 0x1f >= 1
    };
    if intel && pmu {
        // SAFETY: architectural PMU MSRs CPUID leaf 0AH enumerated; fixed counter 0's
        // nibble is bits 3:0 (OS and USR), its enable bit 32 of the global control.
        unsafe {
            cpu::wrmsr(IA32_FIXED_CTR_CTRL, cpu::rdmsr(IA32_FIXED_CTR_CTRL) | 0b0011);
            cpu::wrmsr(IA32_PERF_GLOBAL_CTRL, cpu::rdmsr(IA32_PERF_GLOBAL_CTRL) | 1 << 32);
        }
        bits |= INSTRUCTIONS;
    }
    if intel {
        ENERGY_UNIT.store((cpu::rdmsr(MSR_RAPL_POWER_UNIT) >> 8) & 0x1f, Relaxed);
        bits |= RAPL;
    }
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
        firmware: super::smi_cmd::counted().map(|(calls, nanos)| Firmware { calls, nanos }),
        instructions: msr(INSTRUCTIONS, IA32_FIXED_CTR0),
        pkg_energy_uj: msr(RAPL, MSR_PKG_ENERGY_STATUS).map(|raw| {
            let me = percpu::cpu_id() as usize;
            let raw = raw & 0xFFFF_FFFF;
            let last = ENERGY[me].swap(raw, Relaxed);
            if raw < last {
                ENERGY_WRAPS[me].fetch_add(1, Relaxed);
            }
            let ticks = (ENERGY_WRAPS[me].load(Relaxed) << 32) + raw;
            ((u128::from(ticks) * 1_000_000) >> ENERGY_UNIT.load(Relaxed)) as u64
        }),
    }
}
