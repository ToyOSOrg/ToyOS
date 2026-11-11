//! What each CPU needs against speculative execution and where it stands:
//! the words it reads of itself at bring-up, [`kernel::mitigation::decide`]'s
//! verdict on them once every CPU is up, and one line per vulnerability. The
//! first CPU says all of it; a later one only where its words or its standing
//! differ.
//!
//! [`apply`] sets the per-CPU controls the decision names that the kernel
//! owns here.

use alloc::vec::Vec;
use core::fmt;

use kernel::microcode::Revision;
use kernel::mitigation::{
    decide, Facts, Mitigation, Registers, Vulnerability, C001_1029_LFENCE_SERIALIZES_DISPATCH,
    CPUID_7_0_EDX_ARCH_CAPABILITIES,
};
use kernel::sched::hw::CpuId;

use super::{cpu, microcode};
use crate::log;
use crate::sync::Lock;

const IA32_ARCH_CAPABILITIES: u32 = 0x10A;
/// Intel, "CPUID Enumeration and Architectural MSRs", Table 6.
const IA32_MCU_OPT_CTRL: u32 = 0x123;
/// AMD, "Technical Guidance for Mitigating Branch Type Confusion" §6.2.1.
const DE_CFG2: u32 = 0xC001_10E3;
/// AMD 70586 G-2.
const C001_1029: u32 = 0xC001_1029;

/// What one CPU read of itself at bring-up.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Read {
    /// Its `smt` is [`report`]'s: which CPUs share its core is known once
    /// every CPU is up.
    facts: Facts,
    registers: Registers,
    /// C001_1029 as [`apply`] found it, where it wrote it.
    c001_1029_found: Option<u64>,
}

/// Each CPU's [`Read`], by CPU id.
static READ: Lock<Vec<Option<Read>>> = Lock::new(Vec::new());

/// This CPU's words, all but `smt`.
fn words() -> Facts {
    let max = cpu::cpuid(0, 0).0;
    let max_ext = cpu::cpuid(0x8000_0000, 0).0;
    let (signature, _, ecx, _) = cpu::cpuid(1, 0);
    let (_, cpuid_7_0_ebx, _, cpuid_7_0_edx) = cpu::leaf_7();
    // CPUID.(07H,0):EAX is the leaf's highest subleaf.
    let cpuid_7_2_edx = if max >= 7 && cpu::cpuid(7, 0).0 >= 2 { cpu::cpuid(7, 2).3 } else { 0 };
    let arch_capabilities =
        if cpuid_7_0_edx & CPUID_7_0_EDX_ARCH_CAPABILITIES != 0 { cpu::rdmsr(IA32_ARCH_CAPABILITIES) } else { 0 };
    let (cpuid_8000_0021_eax, _, cpuid_8000_0021_ecx, _) =
        if max_ext >= 0x8000_0021 { cpu::cpuid(0x8000_0021, 0) } else { (0, 0, 0, 0) };
    Facts {
        vendor: cpu::vendor(),
        signature,
        hypervisor: ecx & kernel::cpu::CPUID_1_ECX_HYPERVISOR != 0,
        smt: false,
        microcode: microcode::running().unwrap_or(Revision(0)),
        cpuid_7_0_ebx,
        cpuid_7_0_edx,
        cpuid_7_2_edx,
        arch_capabilities,
        cpuid_8000_0008_ebx: if max_ext >= 0x8000_0008 { cpu::cpuid(0x8000_0008, 0).1 } else { 0 },
        cpuid_8000_0021_eax,
        cpuid_8000_0021_ecx,
    }
}

/// Sets this CPU's controls the decision on its words names, once its
/// microcode has loaded and before anything relies on them, and returns
/// C001_1029 as it found it where it wrote it. That is LFENCE dispatch
/// serializing, on which the clocksource's ordered TSC read (`RDTSCP;
/// LFENCE`) relies as a bounds-check barrier does: a CPU where the bit does
/// not hold stops the machine, as one without SMEP does.
pub fn apply() -> Option<u64> {
    if !decide(&words()).enumerates(Mitigation::LfenceDispatchSerializing) {
        return None;
    }
    let found = cpu::rdmsr(C001_1029);
    // SAFETY: the decision found the MSR on this CPU's family, outside a
    // guest; the bit changes only how LFENCE dispatches.
    unsafe { cpu::wrmsr(C001_1029, found | C001_1029_LFENCE_SERIALIZES_DISPATCH) };
    let now = cpu::rdmsr(C001_1029);
    assert!(
        now & C001_1029_LFENCE_SERIALIZES_DISPATCH != 0,
        "mitigation: C001_1029 reads {now:#x} once LFENCE dispatch serializing was set, and the ordered TSC read needs it"
    );
    Some(found)
}

/// Records this CPU's words and registers, once [`apply`] has set what it
/// sets. Which registers it has does not depend on its core's other threads.
pub fn record(cpu_id: u32, c001_1029_found: Option<u64>) {
    let facts = words();
    let decision = decide(&facts);
    let read = |enumerated: bool, msr: u32| enumerated.then(|| cpu::rdmsr(msr));
    let registers = Registers {
        mcu_opt_ctrl: read(
            decision.enumerates(Mitigation::SrbdsMicrocode) || decision.enumerates(Mitigation::GdsMicrocode),
            IA32_MCU_OPT_CTRL,
        ),
        de_cfg2: read(decision.enumerates(Mitigation::SuppressBpOnNonBr), DE_CFG2),
        c001_1029: read(decision.enumerates(Mitigation::LfenceDispatchSerializing), C001_1029),
    };
    let mut all = READ.lock();
    let at = cpu_id as usize;
    if all.len() <= at {
        all.resize(at + 1, None);
    }
    all[at] = Some(Read { facts, registers, c001_1029_found });
}

/// Says each online CPU's words and lines once bring-up has installed the
/// topology: the first CPU's, and a later one's where they differ. A CPU's
/// `smt` is whether another online CPU shares its core, by the cores the
/// topology places their APIC IDs in: CPUID.0BH/1FH EBX[15:0] is the count
/// as shipped and "must not" enumerate the topology (SDM Vol. 1, Chapter 21).
pub fn report() {
    let topology = crate::smp::topology();
    let read = READ.lock();
    let mut first = None;
    for cpu_id in 0..topology.cpus() as u32 {
        let read = read
            .get(cpu_id as usize)
            .copied()
            .flatten()
            .unwrap_or_else(|| panic!("mitigation: cpu{cpu_id} is online and recorded no words at bring-up"));
        let said = Read { facts: Facts { smt: topology.core(CpuId(cpu_id)).len() > 1, ..read.facts }, ..read };
        match first {
            Some(first) if first == said => continue,
            Some(_) => {}
            None => first = Some(said),
        }
        say(cpu_id, &said);
    }
}

fn say(cpu_id: u32, read: &Read) {
    let Read { facts, registers, c001_1029_found } = *read;
    let Facts {
        vendor,
        signature,
        hypervisor,
        smt,
        microcode,
        cpuid_7_0_ebx,
        cpuid_7_0_edx,
        cpuid_7_2_edx,
        arch_capabilities,
        cpuid_8000_0008_ebx,
        cpuid_8000_0021_eax,
        cpuid_8000_0021_ecx,
    } = facts;
    let Registers { mcu_opt_ctrl, de_cfg2, c001_1029 } = registers;
    log!(
        "mitigations: cpu{cpu_id} words {vendor:?} signature={signature:#x} microcode={microcode:#x} hypervisor={hypervisor} \
         smt={smt} cpuid.7.0.ebx={cpuid_7_0_ebx:#x} cpuid.7.0.edx={cpuid_7_0_edx:#x} cpuid.7.2.edx={cpuid_7_2_edx:#x} \
         arch_capabilities={arch_capabilities:#x} cpuid.80000008.ebx={cpuid_8000_0008_ebx:#x} \
         cpuid.80000021.eax={cpuid_8000_0021_eax:#x} cpuid.80000021.ecx={cpuid_8000_0021_ecx:#x} \
         mcu_opt_ctrl={} de_cfg2={} c001_1029={} c001_1029_found={}",
        Msr(mcu_opt_ctrl),
        Msr(de_cfg2),
        Msr(c001_1029),
        Msr(c001_1029_found),
    );
    let decision = decide(&facts);
    let in_force = decision.in_force(&registers);
    for of in Vulnerability::ALL {
        log!("mitigations: cpu{cpu_id} {}: {}", of.name(), decision.line(of, in_force));
    }
}

/// A register as the words line gives it: `-` where it was not read.
struct Msr(Option<u64>);

impl fmt::Display for Msr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(f, "{value:#x}"),
            None => f.write_str("-"),
        }
    }
}
