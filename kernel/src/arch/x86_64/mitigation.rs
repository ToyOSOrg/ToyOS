//! What this CPU needs against speculative execution and where it stands:
//! the words it reads of itself, [`kernel::mitigation::decide`]'s verdict on
//! them, and one line per vulnerability. The first CPU says all of it; a
//! later one only where its words or its standing differ.
//!
//! [`apply`] sets the per-CPU controls the decision names that the kernel
//! owns here. What is in force is read back from this CPU's registers, beside
//! what the kernel does on every CPU already.

use kernel::mitigation::{decide, Decision, Facts, Mitigation, Mitigations, Vulnerability, CPUID_7_0_EDX_ARCH_CAPABILITIES};
use kernel::microcode::Revision;

use super::{cpu, microcode};
use crate::log;
use crate::sync::Lock;

const IA32_ARCH_CAPABILITIES: u32 = 0x10A;
/// `IA32_MCU_OPT_CTRL` and its two opt-outs (Intel, "CPUID Enumeration and
/// Architectural MSRs", Table 6).
const IA32_MCU_OPT_CTRL: u32 = 0x123;
const RNGDS_MITG_DIS: u64 = 1 << 0;
const GDS_MITG_DIS: u64 = 1 << 4;
/// AMD's `DE_CFG2` and `SuppressBPOnNonBr` ("Technical Guidance for
/// Mitigating Branch Type Confusion" §6.2.1).
const DE_CFG2: u32 = 0xC001_10E3;
const SUPPRESS_BP_ON_NON_BR: u64 = 1 << 1;
/// AMD's MSR C001_1029, whose bit 1 makes LFENCE dispatch serializing
/// (70586 G-2).
const LFENCE_CONTROL: u32 = 0xC001_1029;
const LFENCE_SERIALIZES_DISPATCH: u64 = 1 << 1;

/// The first CPU's words and standing.
static FIRST: Lock<Option<(Facts, Mitigations)>> = Lock::new(None);

fn facts() -> Facts {
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
        smt: cpu::threads_per_core() > 1,
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

/// What is in force on this CPU of what `decision` needs.
fn applied(decision: &Decision) -> Mitigations {
    // `arch::uaccess` clamps every user pointer, `control_regs` refuses a
    // CPU without `CR4.SMEP`, and nothing enters VMX or SVM operation.
    let mut applied = Mitigations::of(&[Mitigation::UserPointerClamp, Mitigation::Smep, Mitigation::NoGuest]);
    // Each register is read only where the decision found its control
    // enumerated: an absent MSR is `#GP`. Intel's two bits opt out of a
    // mitigation its microcode turns on; AMD's turns one on.
    let srbds = decision.enumerates(Mitigation::SrbdsMicrocode);
    let gds = decision.enumerates(Mitigation::GdsMicrocode);
    if srbds || gds {
        let opt_ctrl = cpu::rdmsr(IA32_MCU_OPT_CTRL);
        if srbds && opt_ctrl & RNGDS_MITG_DIS == 0 {
            applied = applied.with(Mitigation::SrbdsMicrocode);
        }
        if gds && opt_ctrl & GDS_MITG_DIS == 0 {
            applied = applied.with(Mitigation::GdsMicrocode);
        }
    }
    if decision.enumerates(Mitigation::SuppressBpOnNonBr) && cpu::rdmsr(DE_CFG2) & SUPPRESS_BP_ON_NON_BR != 0 {
        applied = applied.with(Mitigation::SuppressBpOnNonBr);
    }
    if decision.enumerates(Mitigation::LfenceDispatchSerializing)
        && cpu::rdmsr(LFENCE_CONTROL) & LFENCE_SERIALIZES_DISPATCH != 0
    {
        applied = applied.with(Mitigation::LfenceDispatchSerializing);
    }
    applied
}

/// Sets this CPU's controls the decision on its words names, once its
/// microcode has loaded and before anything relies on them: LFENCE dispatch
/// serializing, on which the kernel's ordered TSC read and x2APIC write
/// fence rely as much as a bounds-check barrier does.
pub fn apply() {
    if decide(&facts()).enumerates(Mitigation::LfenceDispatchSerializing) {
        // SAFETY: the decision found the MSR on this CPU's family, outside a
        // guest; the bit changes only how LFENCE dispatches.
        unsafe { cpu::wrmsr(LFENCE_CONTROL, cpu::rdmsr(LFENCE_CONTROL) | LFENCE_SERIALIZES_DISPATCH) };
    }
}

/// Says this CPU's words and lines, if it is the first or differs from it.
pub fn report(cpu_id: u32) {
    let facts = facts();
    let decision = decide(&facts);
    let applied = applied(&decision);
    {
        let mut first = FIRST.lock();
        match *first {
            Some(said) if said == (facts, applied) => return,
            Some(_) => {}
            None => *first = Some((facts, applied)),
        }
    }
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
    log!(
        "mitigations: cpu{cpu_id} words {vendor:?} signature={signature:#x} microcode={microcode:#x} hypervisor={hypervisor} \
         smt={smt} cpuid.7.0.ebx={cpuid_7_0_ebx:#x} cpuid.7.0.edx={cpuid_7_0_edx:#x} cpuid.7.2.edx={cpuid_7_2_edx:#x} \
         arch_capabilities={arch_capabilities:#x} cpuid.80000008.ebx={cpuid_8000_0008_ebx:#x} \
         cpuid.80000021.eax={cpuid_8000_0021_eax:#x} cpuid.80000021.ecx={cpuid_8000_0021_ecx:#x}"
    );
    for of in Vulnerability::ALL {
        log!("mitigations: cpu{cpu_id} {}: {}", of.name(), decision.line(of, applied));
    }
}
