//! Which of this CPU's model-specific counters it reads: decided on the CPU
//! itself at bring-up, by `toyos_cpuvuln::counters` over its own CPUID, and
//! the power envelope where `control_regs` declared it.

use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use toyos_abi::counters::Counter;
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
/// MEASUREMENT ONLY: the scout's model-specific registers and its two HITM
/// counters, on Tiger Lake alone.
const SCOUT: u8 = 1 << 4;

/// MEASUREMENT ONLY, SDM Vol. 3B §20.2.2 and Vol. 4 §2.17.
const IA32_FIXED_CTR0: u32 = 0x309;
const MSR_RAPL_POWER_UNIT: u32 = 0x606;
const MSR_PKG_ENERGY_STATUS: u32 = 0x611;

/// MEASUREMENT ONLY: the scout's registers, the residencies as Linux reads them
/// on this model (`arch/x86/events/intel/cstate.c`'s `icl_cstates`, which
/// `TIGERLAKE_L` takes), SDM Vol. 4 §2.17 and Table 2-2.
const IA32_PMC0: u32 = 0xC1;
const IA32_PMC1: u32 = 0xC2;
const IA32_PERFEVTSEL0: u32 = 0x186;
const IA32_PERFEVTSEL1: u32 = 0x187;
const MSR_OFFCORE_RSP_0: u32 = 0x1A6;
const IA32_PERF_STATUS: u32 = 0x198;
const IA32_PERF_GLOBAL_CTRL: u32 = 0x38F;
const MSR_CORE_C6_RESIDENCY: u32 = 0x3FD;
const MSR_CORE_C7_RESIDENCY: u32 = 0x3FE;
const MSR_PKG_C2_RESIDENCY: u32 = 0x60D;
const MSR_PKG_C3_RESIDENCY: u32 = 0x3F8;
const MSR_PKG_C6_RESIDENCY: u32 = 0x3F9;
const MSR_PKG_C7_RESIDENCY: u32 = 0x3FA;
const MSR_PKG_C8_RESIDENCY: u32 = 0x630;
const MSR_PKG_C9_RESIDENCY: u32 = 0x631;
const MSR_PKG_C10_RESIDENCY: u32 = 0x632;
const MSR_PKG_POWER_LIMIT: u32 = 0x610;
const MSR_CORE_PERF_LIMIT_REASONS: u32 = 0x64F;
const IA32_HWP_CAPABILITIES: u32 = 0x771;
const IA32_HWP_STATUS: u32 = 0x777;
const MSR_POWER_CTL: u32 = 0x1FC;
const MSR_PKG_CST_CONFIG_CONTROL: u32 = 0xE2;

/// Family 6 models 8CH (`TIGERLAKE_L`, the T14's i5-1135G7) and 8DH.
const TIGER_LAKE: [u32; 2] = [0x8C, 0x8D];

/// `IA32_PERFEVTSELx`: event, umask, USR (16), OS (17), EN (22).
const fn evtsel(event: u64, umask: u64) -> u64 {
    event | umask << 8 | 1 << 16 | 1 << 17 | 1 << 22
}
/// `MEM_LOAD_L3_HIT_RETIRED.XSNP_HITM`, Intel's Tiger Lake event list.
const HITM_LOADS: u64 = evtsel(0xD2, 0x04);
/// `OCR.DEMAND_RFO.L3_HIT.SNOOP_HITM`: the off-core response event on
/// `MSR_OFFCORE_RSP_0`, with the response it matches written there.
const OCR_RSP0: u64 = evtsel(0xB7, 0x01);
const DEMAND_RFO_L3_HIT_SNOOP_HITM: u64 = 0x10_003C_0002;
/// Fixed counter 0's nibble of `IA32_FIXED_CTR_CTRL` (OS, USR), and its, PMC0's
/// and PMC1's enables in `IA32_PERF_GLOBAL_CTRL`.
const FIXED_CTR0_COUNTING: u64 = 0b0011;
const GLOBAL_FIXED_CTR0: u64 = 1 << 32;
const GLOBAL_PMC0_PMC1: u64 = 0b11;

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
    let (pmu, general) = if cpu::cpuid(0, 0).0 >= 0x0A {
        let (eax, _, _, edx) = cpu::cpuid(0x0A, 0);
        (eax & 0xff >= 2 && edx & 0x1f >= 1, (eax >> 8) & 0xff)
    } else {
        (false, 0)
    };
    // Family 6's display model is the extended model above the model.
    let model = (signature >> 12 & 0xF0) | (signature >> 4 & 0x0F);
    let tiger_lake = intel && signature >> 8 & 0xF == 6 && TIGER_LAKE.contains(&model);
    let mut global = 0;
    if intel && pmu {
        global |= GLOBAL_FIXED_CTR0;
        bits |= INSTRUCTIONS;
    }
    if tiger_lake && pmu && general >= 2 {
        // SAFETY: the two general-purpose counters CPUID leaf 0AH enumerated,
        // each stopped before it is set up, and the off-core response register
        // this model's event list names for event B7H.
        unsafe {
            cpu::wrmsr(IA32_PERFEVTSEL0, 0);
            cpu::wrmsr(IA32_PERFEVTSEL1, 0);
            cpu::wrmsr(IA32_PMC0, 0);
            cpu::wrmsr(IA32_PMC1, 0);
            cpu::wrmsr(MSR_OFFCORE_RSP_0, DEMAND_RFO_L3_HIT_SNOOP_HITM);
            cpu::wrmsr(IA32_PERFEVTSEL0, HITM_LOADS);
            cpu::wrmsr(IA32_PERFEVTSEL1, OCR_RSP0);
        }
        global |= GLOBAL_PMC0_PMC1;
        bits |= SCOUT;
    }
    if global != 0 {
        // The watchdog's `pmu::arm`, after this on every CPU, writes both
        // control registers whole: it carries these.
        super::pmu::measure(if bits & INSTRUCTIONS != 0 { FIXED_CTR0_COUNTING } else { 0 }, global);
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

/// MEASUREMENT ONLY: one of the scout's counters on this CPU, read once per
/// sample. The two logs are cleared as they are read, so each read says what
/// was logged since the one before on this CPU (`IA32_HWP_STATUS`) or on its
/// package (`MSR_CORE_PERF_LIMIT_REASONS`, whose log bits SDM Vol. 4 names
/// R/WC0).
pub fn scout(counter: Counter) -> Option<u64> {
    if ADMITTED[percpu::cpu_id() as usize].load(Relaxed) & SCOUT == 0 {
        return None;
    }
    let at = match counter {
        Counter::CoreC6 => MSR_CORE_C6_RESIDENCY,
        Counter::CoreC7 => MSR_CORE_C7_RESIDENCY,
        Counter::PkgC2 => MSR_PKG_C2_RESIDENCY,
        Counter::PkgC3 => MSR_PKG_C3_RESIDENCY,
        Counter::PkgC6 => MSR_PKG_C6_RESIDENCY,
        Counter::PkgC7 => MSR_PKG_C7_RESIDENCY,
        Counter::PkgC8 => MSR_PKG_C8_RESIDENCY,
        Counter::PkgC9 => MSR_PKG_C9_RESIDENCY,
        Counter::PkgC10 => MSR_PKG_C10_RESIDENCY,
        Counter::PerfStatus => IA32_PERF_STATUS,
        Counter::HwpCapabilities => IA32_HWP_CAPABILITIES,
        Counter::PkgPowerLimit => MSR_PKG_POWER_LIMIT,
        Counter::RaplPowerUnit => MSR_RAPL_POWER_UNIT,
        Counter::PmuGlobalCtrl => IA32_PERF_GLOBAL_CTRL,
        Counter::PowerCtl => MSR_POWER_CTL,
        Counter::PkgCstConfig => MSR_PKG_CST_CONFIG_CONTROL,
        Counter::HitmLoads => IA32_PMC0,
        Counter::HitmRfos => IA32_PMC1,
        Counter::HwpStatus | Counter::PerfLimitReasons => {
            let at = if counter == Counter::HwpStatus { IA32_HWP_STATUS } else { MSR_CORE_PERF_LIMIT_REASONS };
            let value = cpu::rdmsr(at);
            // SAFETY: both registers' log bits are cleared by writing 0
            // (SDM Vol. 4: `IA32_HWP_STATUS` and `MSR_CORE_PERF_LIMIT_REASONS`),
            // and the rest of each is read-only or reserved as zero.
            unsafe { cpu::wrmsr(at, 0) };
            return Some(value);
        }
        _ => return None,
    };
    Some(cpu::rdmsr(at))
}
