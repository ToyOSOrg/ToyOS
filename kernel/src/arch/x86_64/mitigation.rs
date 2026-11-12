//! What each CPU needs against speculative execution and where it stands:
//! the words it reads of itself at bring-up, [`kernel::mitigation::decide`]'s
//! verdict on them once every CPU is up, and one line per vulnerability. The
//! first CPU says all of it; a later one only where its words or its standing
//! differ.
//!
//! [`apply`] sets the per-CPU controls the decision names that the kernel
//! owns here: `IA32_SPEC_CTRL` whole, the named `IA32_MCU_OPT_CTRL`
//! opt-outs and AMD's LFENCE bit. The boot CPU runs it before the allocator
//! is up, so what it returns and where the boot CPU keeps it hold no heap.
//! [`switch_to`] applies at each switch what the decision names for a program
//! that asked for isolated speculation.

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use kernel::microcode::Revision;
use kernel::mitigation::{
    decide, Facts, LastAsked, Mitigation, OnRequest, Registers, Speculation, Vulnerability,
    C001_1029_LFENCE_SERIALIZES_DISPATCH, CPUID_7_0_EDX_ARCH_CAPABILITIES,
};
use kernel::sched::hw::CpuId;
use toyos_abi::Pid;

use super::{cpu, microcode};
use crate::cpu_slice::CpuSlice;
use crate::log;
use crate::sync::Lock;

const IA32_ARCH_CAPABILITIES: u32 = 0x10A;
/// Intel, "CPUID Enumeration and Architectural MSRs", Table 3; AMD's
/// `SPEC_CTRL` at the same address.
const IA32_SPEC_CTRL: u32 = 0x48;
/// Intel, "CPUID Enumeration and Architectural MSRs", Table 6.
const IA32_MCU_OPT_CTRL: u32 = 0x123;
/// `IA32_MCU_OPT_CTRL.GDS_MITG_DIS`: Intel, "Gather Data Sampling", Table 3.
const GDS_MITG_DIS: u64 = 1 << 4;
/// AMD, "Technical Guidance for Mitigating Branch Type Confusion" §6.2.1.
const DE_CFG2: u32 = 0xC001_10E3;
/// AMD 70586 G-2.
const C001_1029: u32 = 0xC001_1029;
/// `IA32_PRED_CMD`: Intel, "CPUID Enumeration and Architectural MSRs",
/// Table 4; AMD's `PRED_CMD` at the same address. Bit 0 is IBPB.
const IA32_PRED_CMD: u32 = 0x49;
const IBPB: u64 = 1 << 0;
/// AMD's `VIRT_SPEC_CTRL` (APM Vol. 3 Fn8000_0008_EBX[SsbdVirtSpecCtrl]).
const VIRT_SPEC_CTRL: u32 = 0xC001_011F;

/// What one CPU read of itself at bring-up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Read {
    /// Its `smt` is [`report`]'s: which CPUs share its core is known once
    /// every CPU is up.
    facts: Facts,
    registers: Registers,
    /// Each register [`apply`] writes, as it found it.
    found: Registers,
}

/// The boot CPU's [`Read`].
static BOOT_CPU: Lock<Option<Read>> = Lock::new(None);
/// Each later CPU's [`Read`], by CPU id: sized with the other per-CPU tables.
static LATER: CpuSlice<Lock<Option<Read>>> = CpuSlice::new();

/// A slot for each of the `cpus` CPUs the firmware names.
pub fn size(cpus: usize) {
    LATER.size(cpus, |_| Lock::new(None));
}

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
/// microcode has loaded and before anything relies on them, and returns what
/// it read of itself. The clocksource's ordered TSC read (`RDTSCP; LFENCE`)
/// relies on LFENCE dispatch serializing as a bounds-check barrier does: a
/// CPU where that bit does not hold stops the machine, as one without SMEP
/// does. Any other write that does not take is reported by the lines, not
/// stopped. Which registers it reads does not depend on its core's other
/// threads.
pub fn apply() -> Read {
    let facts = words();
    let decision = decide(&facts);
    let lfence = decision.enumerates(Mitigation::LfenceDispatchSerializing);
    let mcu_opt_ctrl =
        decision.enumerates(Mitigation::SrbdsMicrocode) || decision.enumerates(Mitigation::GdsMicrocode);
    if decision.enumerates(Mitigation::GdsMicrocode) && crate::actuator::gds_found_disabled() {
        // SAFETY: GDS_CTRL enumerates the bit; the write below clears it.
        unsafe { cpu::wrmsr(IA32_MCU_OPT_CTRL, cpu::rdmsr(IA32_MCU_OPT_CTRL) | GDS_MITG_DIS) };
    }
    let read = |enumerated: bool, msr: u32| enumerated.then(|| cpu::rdmsr(msr));
    let found = Registers {
        spec_ctrl: read(facts.has_spec_ctrl(), IA32_SPEC_CTRL),
        mcu_opt_ctrl: read(mcu_opt_ctrl, IA32_MCU_OPT_CTRL),
        de_cfg2: None,
        c001_1029: read(lfence, C001_1029),
    };
    // SAFETY: each write names only bits the CPU enumerates, and each turns a
    // mitigation on: `IA32_SPEC_CTRL`'s while no program that asked runs, the
    // microcode's by clearing its opt-out, and LFENCE's dispatch ordering,
    // whose MSR the decision found on this CPU's family outside a guest.
    unsafe {
        if found.spec_ctrl.is_some() {
            cpu::wrmsr(IA32_SPEC_CTRL, decision.spec_ctrl());
        }
        if let Some(opt) = found.mcu_opt_ctrl.filter(|opt| opt & decision.mcu_opt_ctrl_clears() != 0) {
            cpu::wrmsr(IA32_MCU_OPT_CTRL, opt & !decision.mcu_opt_ctrl_clears());
        }
        if let Some(c001_1029) = found.c001_1029 {
            cpu::wrmsr(C001_1029, c001_1029 | C001_1029_LFENCE_SERIALIZES_DISPATCH);
        }
    }
    let registers = Registers {
        spec_ctrl: read(found.spec_ctrl.is_some(), IA32_SPEC_CTRL),
        mcu_opt_ctrl: read(mcu_opt_ctrl, IA32_MCU_OPT_CTRL),
        de_cfg2: read(decision.enumerates(Mitigation::SuppressBpOnNonBr), DE_CFG2),
        c001_1029: read(lfence, C001_1029),
    };
    if let Some(now) = registers.c001_1029 {
        assert!(
            now & C001_1029_LFENCE_SERIALIZES_DISPATCH != 0,
            "mitigation: C001_1029 reads {now:#x} once LFENCE dispatch serializing was set, and the ordered TSC read needs it"
        );
    }
    Read { facts, registers, found }
}

/// Keeps the boot CPU's [`apply`], which it takes before anything is sized.
pub fn record_boot_cpu(read: Read) {
    *BOOT_CPU.lock() = Some(read);
}

/// Keeps a later CPU's [`apply`].
pub fn record(cpu_id: u32, read: Read) {
    *LATER[cpu_id as usize].lock() = Some(read);
}

/// Each online CPU's [`Read`] once bring-up has installed the topology, with
/// its `smt`: whether another online CPU shares its core, by the cores the
/// topology places their APIC IDs in. CPUID.0BH/1FH EBX[15:0] is the count
/// as shipped and "must not" enumerate the topology (SDM Vol. 1, Chapter 21).
fn online() -> impl Iterator<Item = (u32, Read)> {
    let topology = crate::smp::topology();
    (0..topology.cpus() as u32).map(move |cpu_id| {
        let read = match cpu_id {
            0 => *BOOT_CPU.lock(),
            later => *LATER[later as usize].lock(),
        };
        let read = read.unwrap_or_else(|| panic!("mitigation: cpu{cpu_id} is online and recorded no words at bring-up"));
        (cpu_id, Read { facts: Facts { smt: topology.core(CpuId(cpu_id)).len() > 1, ..read.facts }, ..read })
    })
}

/// Gives each online CPU what [`switch_to`] applies there, once SMT is known
/// and before any program runs: what its decision names on request, and the
/// `IA32_SPEC_CTRL` its bring-up left.
pub fn install() {
    for (cpu_id, read) in online() {
        let local = &crate::cpu_local::of(cpu_id).arch.mitigation;
        local.install(read.registers.spec_ctrl, decide(&read.facts).on_request());
    }
}

/// Says each online CPU's words and lines: the first CPU's, and a later
/// one's where they differ.
pub fn report() {
    let mut first = None;
    for (cpu_id, said) in online() {
        match first {
            Some(first) if first == said => continue,
            Some(_) => {}
            None => first = Some(said),
        }
        say(cpu_id, &said);
    }
}

fn say(cpu_id: u32, read: &Read) {
    let Read { facts, registers, found } = *read;
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
    let Registers { spec_ctrl, mcu_opt_ctrl, de_cfg2, c001_1029 } = registers;
    log!(
        "mitigations: cpu{cpu_id} words {vendor:?} signature={signature:#x} microcode={microcode:#x} hypervisor={hypervisor} \
         smt={smt} cpuid.7.0.ebx={cpuid_7_0_ebx:#x} cpuid.7.0.edx={cpuid_7_0_edx:#x} cpuid.7.2.edx={cpuid_7_2_edx:#x} \
         arch_capabilities={arch_capabilities:#x} cpuid.80000008.ebx={cpuid_8000_0008_ebx:#x} \
         cpuid.80000021.eax={cpuid_8000_0021_eax:#x} cpuid.80000021.ecx={cpuid_8000_0021_ecx:#x} \
         spec_ctrl={} mcu_opt_ctrl={} de_cfg2={} c001_1029={} \
         spec_ctrl_found={} mcu_opt_ctrl_found={} c001_1029_found={}",
        Msr(spec_ctrl),
        Msr(mcu_opt_ctrl),
        Msr(de_cfg2),
        Msr(c001_1029),
        Msr(found.spec_ctrl),
        Msr(found.mcu_opt_ctrl),
        Msr(found.c001_1029),
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

/// This CPU's half of [`switch_to`]. What its decision applies for a
/// program that asked, [`install`] writes once every CPU is up and before any
/// program runs; what the CPU last loaded, its own switches alone write.
pub struct Local {
    /// Whether the decision names anything on request: where it does not,
    /// [`switch_to`] has nothing to do.
    applies: AtomicBool,
    /// `IA32_SPEC_CTRL` as this CPU's bring-up left it, which a program that
    /// did not ask runs with.
    spec_ctrl: AtomicU64,
    /// [`OnRequest::spec_ctrl`].
    asked_spec_ctrl: AtomicU64,
    /// [`OnRequest::virt_spec_ctrl`].
    asked_virt_spec_ctrl: AtomicU64,
    /// [`OnRequest::ibpb`].
    ibpb: AtomicBool,
    /// [`OnRequest::fill_after_ibpb`].
    fill: AtomicBool,
    /// [`LastAsked`]'s space, by its raw pid; [`Pid::MAX`], which no process
    /// is issued, for none.
    last_asked: AtomicU32,
    /// Whether the asked bits are loaded.
    loaded: AtomicBool,
    /// IBPBs [`switch_to`] issued on this CPU.
    #[cfg(feature = "test-actuators")]
    ibpbs: AtomicU64,
    /// Whether this CPU has an `IA32_SPEC_CTRL` to read.
    #[cfg(feature = "test-actuators")]
    has_spec_ctrl: AtomicBool,
}

impl Local {
    pub const fn new() -> Self {
        Self {
            applies: AtomicBool::new(false),
            spec_ctrl: AtomicU64::new(0),
            asked_spec_ctrl: AtomicU64::new(0),
            asked_virt_spec_ctrl: AtomicU64::new(0),
            ibpb: AtomicBool::new(false),
            fill: AtomicBool::new(false),
            last_asked: AtomicU32::new(Pid::MAX.0),
            loaded: AtomicBool::new(false),
            #[cfg(feature = "test-actuators")]
            ibpbs: AtomicU64::new(0),
            #[cfg(feature = "test-actuators")]
            has_spec_ctrl: AtomicBool::new(false),
        }
    }

    fn install(&self, spec_ctrl: Option<u64>, on_request: OnRequest) {
        self.applies.store(on_request != OnRequest::default(), Relaxed);
        self.spec_ctrl.store(spec_ctrl.unwrap_or(0), Relaxed);
        #[cfg(feature = "test-actuators")]
        self.has_spec_ctrl.store(spec_ctrl.is_some(), Relaxed);
        self.asked_spec_ctrl.store(on_request.spec_ctrl, Relaxed);
        self.asked_virt_spec_ctrl.store(on_request.virt_spec_ctrl, Relaxed);
        self.ibpb.store(on_request.ibpb, Relaxed);
        self.fill.store(on_request.fill_after_ibpb, Relaxed);
    }
}

/// Applies on this CPU what a switch to `next`, a program's process and its
/// ask or `None` for the idle context, owes by the CPU's decision: an IBPB
/// between two address spaces one of which asked, the return address
/// predictor filled after it where the decision names that, and the asked
/// bits of `IA32_SPEC_CTRL` and `VIRT_SPEC_CTRL` set while a program that
/// asked runs and clear otherwise. Idle is among the otherwise, as Intel asks
/// of STIBP and SSBD before a sleep state on a CPU whose threads share a
/// core ("Speculative Execution Side Channel Mitigations" §2.4.2, §4.1.2).
///
/// A pid names one address space: `toyos_proclife::pids` issues none twice
/// for the machine's life, and a process keeps one space for its own.
pub fn switch_to(next: Option<(Pid, Speculation)>) {
    let local = &crate::cpu_local::here().arch.mitigation;
    if !local.applies.load(Relaxed) {
        return;
    }
    let raw = local.last_asked.load(Relaxed);
    let mut last = LastAsked::new((raw != Pid::MAX.0).then_some(Pid(raw)));
    if last.switch_to(next) && local.ibpb.load(Relaxed) {
        // SAFETY: the decision found IBPB enumerated on this CPU, and the
        // barrier changes no architectural state.
        unsafe { cpu::wrmsr(IA32_PRED_CMD, IBPB) };
        if local.fill.load(Relaxed) {
            fill_return_predictor();
        }
        #[cfg(feature = "test-actuators")]
        local.ibpbs.fetch_add(1, Relaxed);
    }
    let now = last.space().map_or(Pid::MAX.0, |pid| pid.0);
    if now != raw {
        local.last_asked.store(now, Relaxed);
    }
    let asked = matches!(next, Some((_, Speculation::Isolated)));
    if asked != local.loaded.load(Relaxed) {
        let spec_ctrl = local.asked_spec_ctrl.load(Relaxed);
        let virt_spec_ctrl = local.asked_virt_spec_ctrl.load(Relaxed);
        // SAFETY: each asked bit is one the decision found enumerated, and
        // the rest of `IA32_SPEC_CTRL` is what this CPU's bring-up wrote.
        unsafe {
            if spec_ctrl != 0 {
                let base = local.spec_ctrl.load(Relaxed);
                cpu::wrmsr(IA32_SPEC_CTRL, if asked { base | spec_ctrl } else { base });
            }
            if virt_spec_ctrl != 0 {
                cpu::wrmsr(VIRT_SPEC_CTRL, if asked { virt_spec_ctrl } else { 0 });
            }
        }
        local.loaded.store(asked, Relaxed);
    }
}

/// AMD 70586's V2-3, which AMD-SB-1040 asks after an IBPB that leaves the
/// return address predictor's entries: a call for each of the 32 entries
/// AMD's largest predictor holds, each skipping an `int3` that a return
/// predicted from its entry lands on; then the stack as it was.
#[unsafe(naked)]
extern "sysv64" fn fill_return_predictor() {
    core::arch::naked_asm!(
        ".rept 32",
        "call 2f",
        "int3",
        "2:",
        ".endr",
        "add rsp, 32 * 8",
        "ret",
    );
}

/// `SYS_DEBUG`'s `SPEC_CTRL`: this CPU's `IA32_SPEC_CTRL`, where it has one.
#[cfg(feature = "test-actuators")]
pub fn debug_spec_ctrl() -> Option<u64> {
    let local = &crate::cpu_local::here().arch.mitigation;
    local.has_spec_ctrl.load(Relaxed).then(|| cpu::rdmsr(IA32_SPEC_CTRL))
}

/// `SYS_DEBUG`'s `SPEC_CTRL_ON_REQUEST`: [`OnRequest::spec_ctrl`] on this CPU.
#[cfg(feature = "test-actuators")]
pub fn debug_spec_ctrl_on_request() -> u64 {
    crate::cpu_local::here().arch.mitigation.asked_spec_ctrl.load(Relaxed)
}

/// `SYS_DEBUG`'s `SWITCH_IBPBS`: the IBPBs [`switch_to`] issued on this CPU,
/// where its decision names any.
#[cfg(feature = "test-actuators")]
pub fn debug_switch_ibpbs() -> Option<u64> {
    let local = &crate::cpu_local::here().arch.mitigation;
    local.ibpb.load(Relaxed).then(|| local.ibpbs.load(Relaxed))
}
