//! Which speculative-execution vulnerabilities an x86 CPU has, and what its
//! vendor prescribes for each: [`decide`] reads a CPU's identity and
//! enumeration words and names the mitigations it needs, and
//! [`Decision::line`] says where the CPU stands given the ones in force.
//!
//! Every rule is the vendor's. Intel's are its Software Security Guidance
//! articles, each named at its rule, and its "Affected Processors: Guidance
//! for Security Issues on Intel Processors" table; AMD's are its bulletins
//! and white papers, by number. Enumeration decides where it speaks. Where it
//! does not, Intel's table decides for a CPU it lists, and a CPU it does not
//! list is assumed affected and said to be: the table drops processors past
//! their servicing life without saying they are unaffected. AMD lists
//! affected families only, so an AMD family none of its documents names is
//! unaffected unless it is newer than the ones they do.
//!
//! One policy is ToyOS's own: a protection a vendor prescribes for every
//! program is applied only for a program that asks. Where it stands between
//! programs its verdict marks it as policy; where it protects the kernel from
//! code on the core's other thread, the line names the exposure that leaves.
//!
//! A vulnerability Intel's advisory describes for Intel's processors has no
//! AMD rule, and AMD's none for Intel. No other vendor's guidance is read.

#![forbid(unsafe_code)]

use core::fmt;

use toyos_abi::Pid;

use crate::cpu::{Ident, Vendor};
use crate::microcode::Revision;

/// What one CPU says of itself, read on that CPU after its microcode loads:
/// Intel asks that enumeration be re-read after an update ("CPUID Enumeration
/// and Architectural MSRs"). A leaf the CPU does not reach reads as zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub vendor: Vendor,
    /// CPUID.01H:EAX.
    pub signature: u32,
    /// CPUID.01H:ECX[31]: a hypervisor's guest.
    pub hypervisor: bool,
    /// Another CPU the kernel brought up shares this CPU's core.
    pub smt: bool,
    pub microcode: Revision,
    pub cpuid_7_0_ebx: u32,
    pub cpuid_7_0_edx: u32,
    pub cpuid_7_2_edx: u32,
    /// `IA32_ARCH_CAPABILITIES` (10AH); 0 unless CPUID.(07H,0):EDX[29].
    pub arch_capabilities: u64,
    pub cpuid_8000_0008_ebx: u32,
    pub cpuid_8000_0021_eax: u32,
    pub cpuid_8000_0021_ecx: u32,
}

/// CPUID.(07H,0):EDX[29]: `IA32_ARCH_CAPABILITIES` exists.
pub const CPUID_7_0_EDX_ARCH_CAPABILITIES: u32 = 1 << 29;

// CPUID.(07H,0) and (07H,2), "CPUID Enumeration and Architectural MSRs"
// Table 1; HLE and RTM, Intel TSX, are CPUID.(07H,0):EBX[4] and [11] (SDM
// Vol. 1, Chapter 21).
const HLE: u32 = 1 << 4;
const RTM: u32 = 1 << 11;
const SRBDS_CTRL: u32 = 1 << 9;
const MD_CLEAR: u32 = 1 << 10;
const IBRS_IBPB: u32 = 1 << 26;
const STIBP: u32 = 1 << 27;
const L1D_FLUSH: u32 = 1 << 28;
const SSBD: u32 = 1 << 31;
const PSFD: u32 = 1 << 0;
const IPRED_CTRL: u32 = 1 << 1;
const RRSBA_CTRL: u32 = 1 << 2;
const DDPD_U: u32 = 1 << 3;
const BHI_CTRL: u32 = 1 << 4;

/// `IA32_ARCH_CAPABILITIES`, the same article's Table 2.
mod cap {
    pub const RDCL_NO: u64 = 1 << 0;
    pub const IBRS_ALL: u64 = 1 << 1;
    pub const RSBA: u64 = 1 << 2;
    pub const SSB_NO: u64 = 1 << 4;
    pub const MDS_NO: u64 = 1 << 5;
    pub const IF_PSCHANGE_MC_NO: u64 = 1 << 6;
    pub const TSX_CTRL: u64 = 1 << 7;
    pub const TAA_NO: u64 = 1 << 8;
    pub const SBDR_SSDP_NO: u64 = 1 << 13;
    pub const FBSDP_NO: u64 = 1 << 14;
    pub const PSDP_NO: u64 = 1 << 15;
    pub const FB_CLEAR: u64 = 1 << 17;
    pub const RRSBA: u64 = 1 << 19;
    pub const BHI_NO: u64 = 1 << 20;
    pub const GDS_CTRL: u64 = 1 << 25;
    pub const GDS_NO: u64 = 1 << 26;
    pub const RFDS_NO: u64 = 1 << 27;
    pub const RFDS_CLEAR: u64 = 1 << 28;
    pub const ITS_NO: u64 = 1 << 62;
}

/// AMD's CPUID Fn8000_0008_EBX and Fn8000_0021 (APM Vol. 3, 24594, §E.4.7
/// and §E.4.19).
mod amd {
    pub const IBPB: u32 = 1 << 12;
    pub const IBRS: u32 = 1 << 14;
    pub const STIBP: u32 = 1 << 15;
    pub const IBRS_ALWAYS_ON: u32 = 1 << 16;
    pub const IBRS_PREFERRED: u32 = 1 << 18;
    pub const SSBD: u32 = 1 << 24;
    pub const SSBD_VIRT_SPEC_CTRL: u32 = 1 << 25;
    pub const SSBD_NOT_REQUIRED: u32 = 1 << 26;
    pub const PSFD: u32 = 1 << 28;
    pub const BTC_NO: u32 = 1 << 29;
    pub const IBPB_RET: u32 = 1 << 30;
    pub const LFENCE_ALWAYS_SERIALIZING: u32 = 1 << 2;
    pub const VERW_CLEAR: u32 = 1 << 5;
    pub const AUTOMATIC_IBRS: u32 = 1 << 8;
    pub const SRSO_NO: u32 = 1 << 29;
    pub const SRSO_USER_KERNEL_NO: u32 = 1 << 30;
    pub const TSA_SQ_NO: u32 = 1 << 1;
    pub const TSA_L1_NO: u32 = 1 << 2;
}

/// One thing a vendor prescribes. What applies it, and when, is its own
/// work's; this names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mitigation {
    /// A user address past the user half reaches no load, without a branch
    /// (Intel "Bounds Check Bypass"; AMD 70586 V1-2).
    UserPointerClamp,
    /// MSR C001_1029[1], under which no later instruction dispatches before
    /// an LFENCE is the oldest: AMD 70586 G-2, which its V1-1 barrier needs.
    LfenceDispatchSerializing,
    /// CR4.SMEP, under which no return address user code pushed is fetched
    /// in Ring 0 (Intel "Speculative Execution Side Channel Mitigations";
    /// AMD 70586 G-3).
    Smep,
    /// `IA32_SPEC_CTRL.IBRS` set once and left (the same article, "Enhanced
    /// IBRS").
    EnhancedIbrs,
    /// `EFER.AIBRSE` (APM Vol. 3 `CPUID Fn8000_0021_EAX[AutomaticIBRS]`).
    AutomaticIbrs,
    /// `SPEC_CTRL.IBRS` set once where AMD's CPUID prefers it to software
    /// (`IbrsAlwaysOn`, `IbrsPreferred`).
    IbrsAlwaysOn,
    /// `IA32_SPEC_CTRL.IBRS` set on each entry from a less privileged mode,
    /// which Intel prescribes where a return on an empty RSB takes another
    /// predictor's target and retpoline does not hold ("Return Stack Buffer
    /// Underflow").
    IbrsOnEntry,
    /// Intel "Retpoline: A Branch Target Injection Mitigation"; AMD 70586
    /// V2-1, the generic form AMD-SB-1036 asks for.
    Retpoline,
    /// `PRED_CMD.IBPB` between two programs one of which asks.
    Ibpb,
    /// After that IBPB, a call for each of the return address predictor's
    /// entries, where the IBPB leaves them (AMD-SB-1040; AMD 70586 V2-3).
    ReturnPredictorFilled,
    /// `SPEC_CTRL.STIBP` for a program that asks, against its core's other
    /// thread.
    Stibp,
    /// `IA32_SPEC_CTRL.BHI_DIS_S` (Intel "Branch History Injection and
    /// Intra-mode Branch Target Injection").
    BhiDisS,
    /// The same article's short BHB-clearing sequence on entry.
    BhbClear,
    /// The kernel unmapped while user code runs (Intel "Rogue Data Cache
    /// Load").
    KernelUnmappedInUserMode,
    /// `SPEC_CTRL.SSBD` for a program that asks.
    Ssbd,
    /// AMD's `VIRT_SPEC_CTRL` (C001_011FH) SSBD, a guest's.
    VirtSsbd,
    /// No paging entry that faults terminally names a frame holding a
    /// secret (Intel "L1 Terminal Fault").
    TerminalEntriesNameNoSecret,
    /// `VERW m16` before returning to user mode, where the CPU enumerates
    /// that it overwrites the affected buffers.
    Verw,
    /// After reading a secret over MMIO, a read of non-secret data at the
    /// same cache-line offset on the primary bus (Intel "Processor MMIO
    /// Stale Data Vulnerabilities", "Mitigating PSDP Propagation").
    ReadAfterSecret,
    /// `IA32_TSX_CTRL.RTM_DISABLE` (Intel "TSX Asynchronous Abort").
    RtmDisabled,
    /// The vulnerability needs a guest, and ToyOS runs none.
    NoGuest,
    /// `IA32_MCU_OPT_CTRL.RNGDS_MITG_DIS` clear (Intel "Special Register
    /// Buffer Data Sampling").
    SrbdsMicrocode,
    /// `IA32_MCU_OPT_CTRL.GDS_MITG_DIS` clear (Intel "Gather Data
    /// Sampling").
    GdsMicrocode,
    /// AMD's return thunk and its training on entry ("Technical Guidance
    /// for Mitigating Branch Type Confusion" §6.1.1).
    Jmp2Ret,
    /// `DE_CFG2[SuppressBPOnNonBr]` (C001_10E3H bit 1; the same, §6.2.1).
    SuppressBpOnNonBr,
    /// AMD 70608's Safe-RET.
    SafeRet,
    /// Every indirect branch and return ends in the upper half of its
    /// cache line (Intel "Indirect Target Selection").
    BranchesInUpperHalf,
}

impl Mitigation {
    const ALL: [Self; 27] = [
        Self::UserPointerClamp,
        Self::LfenceDispatchSerializing,
        Self::Smep,
        Self::EnhancedIbrs,
        Self::AutomaticIbrs,
        Self::IbrsAlwaysOn,
        Self::IbrsOnEntry,
        Self::Retpoline,
        Self::Ibpb,
        Self::ReturnPredictorFilled,
        Self::Stibp,
        Self::BhiDisS,
        Self::BhbClear,
        Self::KernelUnmappedInUserMode,
        Self::Ssbd,
        Self::VirtSsbd,
        Self::TerminalEntriesNameNoSecret,
        Self::Verw,
        Self::ReadAfterSecret,
        Self::RtmDisabled,
        Self::NoGuest,
        Self::SrbdsMicrocode,
        Self::GdsMicrocode,
        Self::Jmp2Ret,
        Self::SuppressBpOnNonBr,
        Self::SafeRet,
        Self::BranchesInUpperHalf,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::UserPointerClamp => "user pointers clamped without a branch",
            Self::LfenceDispatchSerializing => "LFENCE dispatch-serializing",
            Self::Smep => "SMEP",
            Self::EnhancedIbrs => "enhanced IBRS",
            Self::AutomaticIbrs => "automatic IBRS",
            Self::IbrsAlwaysOn => "IBRS always on",
            Self::IbrsOnEntry => "IBRS on entry",
            Self::Retpoline => "retpoline",
            Self::Ibpb => "IBPB between programs that ask",
            Self::ReturnPredictorFilled => "return address predictor filled after IBPB",
            Self::Stibp => "STIBP for programs that ask",
            Self::BhiDisS => "BHI_DIS_S",
            Self::BhbClear => "BHB cleared on entry",
            Self::KernelUnmappedInUserMode => "kernel unmapped in user mode",
            Self::Ssbd => "SSBD for programs that ask",
            Self::VirtSsbd => "VIRT_SPEC_CTRL SSBD for programs that ask",
            Self::TerminalEntriesNameNoSecret => "no faulting entry names a secret's frame",
            Self::Verw => "VERW on return to user mode",
            Self::ReadAfterSecret => "a non-secret MMIO read after each secret one",
            Self::RtmDisabled => "RTM disabled",
            Self::NoGuest => "no guest runs",
            Self::SrbdsMicrocode => "microcode's RDRAND and RDSEED mitigation",
            Self::GdsMicrocode => "microcode's gather mitigation",
            Self::Jmp2Ret => "Jmp2Ret",
            Self::SuppressBpOnNonBr => "DE_CFG2[SuppressBPOnNonBr]",
            Self::SafeRet => "Safe-RET",
            Self::BranchesInUpperHalf => "indirect branches in a line's upper half",
        }
    }
}

/// A set of [`Mitigation`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mitigations(u32);

impl Mitigations {
    pub const NONE: Self = Self(0);

    pub const fn of(all: &[Mitigation]) -> Self {
        let mut bits = 0;
        let mut at = 0;
        while at < all.len() {
            bits |= 1 << all[at] as u8;
            at += 1;
        }
        Self(bits)
    }

    pub const fn with(self, one: Mitigation) -> Self {
        Self(self.0 | 1 << one as u8)
    }

    pub const fn has(self, one: Mitigation) -> bool {
        self.0 & 1 << one as u8 != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    const fn and(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// Each one's name, as a line gives it.
impl fmt::Display for Mitigations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Named { set: *self, by_policy: Self::NONE }.fmt(f)
    }
}

/// The vulnerabilities decided, each named as its line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Vulnerability {
    SpectreV1,
    SpectreV2,
    BranchHistoryInjection,
    Retbleed,
    SpecRstackOverflow,
    IndirectTargetSelection,
    SpecStoreBypass,
    Meltdown,
    L1tf,
    Mds,
    TsxAsyncAbort,
    MmioStaleData,
    RegFileDataSampling,
    Srbds,
    GatherDataSampling,
    Tsa,
    ItlbMultihit,
}

impl Vulnerability {
    pub const ALL: [Self; 17] = [
        Self::SpectreV1,
        Self::SpectreV2,
        Self::BranchHistoryInjection,
        Self::Retbleed,
        Self::SpecRstackOverflow,
        Self::IndirectTargetSelection,
        Self::SpecStoreBypass,
        Self::Meltdown,
        Self::L1tf,
        Self::Mds,
        Self::TsxAsyncAbort,
        Self::MmioStaleData,
        Self::RegFileDataSampling,
        Self::Srbds,
        Self::GatherDataSampling,
        Self::Tsa,
        Self::ItlbMultihit,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::SpectreV1 => "spectre_v1",
            Self::SpectreV2 => "spectre_v2",
            Self::BranchHistoryInjection => "branch_history_injection",
            Self::Retbleed => "retbleed",
            Self::SpecRstackOverflow => "spec_rstack_overflow",
            Self::IndirectTargetSelection => "indirect_target_selection",
            Self::SpecStoreBypass => "spec_store_bypass",
            Self::Meltdown => "meltdown",
            Self::L1tf => "l1tf",
            Self::Mds => "mds",
            Self::TsxAsyncAbort => "tsx_async_abort",
            Self::MmioStaleData => "mmio_stale_data",
            Self::RegFileDataSampling => "reg_file_data_sampling",
            Self::Srbds => "srbds",
            Self::GatherDataSampling => "gather_data_sampling",
            Self::Tsa => "tsa",
            Self::ItlbMultihit => "itlb_multihit",
        }
    }
}

/// What a CPU needs against one vulnerability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    NotAffected,
    Affected {
        needs: Mitigations,
        /// Those of `needs` this CPU does not enumerate, so nothing can
        /// apply them.
        lacks: Mitigations,
        /// Those of `needs` the vendor prescribes for every program and
        /// ToyOS applies only for a program that asks: its policy.
        by_policy: Mitigations,
        /// What the kernel stays exposed to on the core's other thread,
        /// which no mitigation in `needs` covers.
        smt: SmtExposure,
        /// No vendor row names this CPU; see the module's rule.
        assumed: bool,
    },
    /// A vendor whose guidance is not read.
    Unknown,
}

/// Code on a core's other thread against the kernel, where the vendor asks
/// `STIBP` of every program or SMT off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtExposure {
    None,
    /// ToyOS's policy sets `STIBP` only for a program that asks, so while a
    /// CPU runs one that has not, the kernel is exposed.
    StibpOnRequest,
    /// The CPU has no `STIBP`: exposed whenever SMT is on.
    NoStibp,
}

/// [`decide`]'s verdict on each [`Vulnerability`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision([Verdict; Vulnerability::ALL.len()]);

impl Decision {
    pub const fn verdict(&self, of: Vulnerability) -> Verdict {
        self.0[of as usize]
    }

    /// Some verdict needs `one` and this CPU enumerates it.
    pub fn enumerates(&self, one: Mitigation) -> bool {
        self.0.iter().any(|verdict| matches!(verdict, Verdict::Affected { needs, lacks, .. } if needs.has(one) && !lacks.has(one)))
    }

    /// Where the CPU stands on `of` with `applied` in force.
    pub const fn line(&self, of: Vulnerability, applied: Mitigations) -> Line {
        Line { verdict: self.verdict(of), applied }
    }

    /// `IA32_SPEC_CTRL` (48H) on this CPU while no program that asked runs:
    /// `IBRS` set once where enhanced IBRS or AMD's IBRS-always-on is named,
    /// and `BHI_DIS_S` where it is. Every other bit is clear.
    pub fn spec_ctrl(&self) -> u64 {
        let mut value = 0;
        if self.enumerates(Mitigation::EnhancedIbrs) || self.enumerates(Mitigation::IbrsAlwaysOn) {
            value |= msr::IBRS;
        }
        if self.enumerates(Mitigation::BhiDisS) {
            value |= msr::BHI_DIS_S;
        }
        value
    }

    /// The `IA32_MCU_OPT_CTRL` (123H) opt-outs the kernel clears: each where
    /// the microcode mitigation it disables is named.
    pub fn mcu_opt_ctrl_clears(&self) -> u64 {
        let mut clears = 0;
        if self.enumerates(Mitigation::SrbdsMicrocode) {
            clears |= msr::RNGDS_MITG_DIS;
        }
        if self.enumerates(Mitigation::GdsMicrocode) {
            clears |= msr::GDS_MITG_DIS;
        }
        clears
    }

    /// What this CPU applies for a program that asked ([`Speculation::Isolated`]):
    /// each control the decision names that it enumerates.
    pub fn on_request(&self) -> OnRequest {
        let bit = |one, bit| if self.enumerates(one) { bit } else { 0 };
        OnRequest {
            spec_ctrl: bit(Mitigation::Ssbd, msr::SSBD) | bit(Mitigation::Stibp, msr::STIBP),
            virt_spec_ctrl: bit(Mitigation::VirtSsbd, msr::SSBD),
            ibpb: self.enumerates(Mitigation::Ibpb),
            fill_after_ibpb: self.enumerates(Mitigation::ReturnPredictorFilled),
        }
    }

    /// What the kernel sets at a CPU's bring-up, this decision names and the
    /// CPU enumerates, and `registers` do not show in force.
    pub fn unapplied(&self, registers: &Registers) -> Mitigations {
        let set = Mitigations::of(&[
            Mitigation::LfenceDispatchSerializing,
            Mitigation::EnhancedIbrs,
            Mitigation::IbrsAlwaysOn,
            Mitigation::BhiDisS,
            Mitigation::SrbdsMicrocode,
            Mitigation::GdsMicrocode,
        ]);
        let named = Mitigation::ALL.into_iter().filter(|&one| set.has(one) && self.enumerates(one));
        let in_force = self.in_force(registers);
        named.filter(|&one| !in_force.has(one)).fold(Mitigations::NONE, Mitigations::with)
    }

    /// What is in force on a CPU whose registers read `registers`: [`ALWAYS`];
    /// what the kernel applies by construction wherever this decision names
    /// it and the CPU enumerates it, which no register shows at bring-up (the
    /// controls a program that asks runs with, [`ON_REQUEST`]); and each
    /// control a register shows on. Intel's two `IA32_MCU_OPT_CTRL` bits opt
    /// out of a mitigation its microcode turns on; the others turn one on.
    pub fn in_force(&self, registers: &Registers) -> Mitigations {
        let mut on = ALWAYS;
        for one in Mitigation::ALL.into_iter().filter(|&one| ON_REQUEST.has(one) && self.enumerates(one)) {
            on = on.with(one);
        }
        if let Some(spec_ctrl) = registers.spec_ctrl {
            for (one, bit) in [
                (Mitigation::EnhancedIbrs, msr::IBRS),
                (Mitigation::IbrsAlwaysOn, msr::IBRS),
                (Mitigation::BhiDisS, msr::BHI_DIS_S),
            ] {
                if self.enumerates(one) && spec_ctrl & bit != 0 {
                    on = on.with(one);
                }
            }
        }
        if let Some(opt) = registers.mcu_opt_ctrl {
            if self.enumerates(Mitigation::SrbdsMicrocode) && opt & msr::RNGDS_MITG_DIS == 0 {
                on = on.with(Mitigation::SrbdsMicrocode);
            }
            if self.enumerates(Mitigation::GdsMicrocode) && opt & msr::GDS_MITG_DIS == 0 {
                on = on.with(Mitigation::GdsMicrocode);
            }
        }
        if registers.de_cfg2.is_some_and(|value| value & msr::SUPPRESS_BP_ON_NON_BR != 0) {
            on = on.with(Mitigation::SuppressBpOnNonBr);
        }
        if registers.c001_1029.is_some_and(|value| value & C001_1029_LFENCE_SERIALIZES_DISPATCH != 0) {
            on = on.with(Mitigation::LfenceDispatchSerializing);
        }
        on
    }
}

/// What the kernel does on every CPU: it clamps every user pointer, refuses a
/// CPU without `CR4.SMEP`, and enters no VMX or SVM operation.
pub const ALWAYS: Mitigations = Mitigations::of(&[Mitigation::UserPointerClamp, Mitigation::Smep, Mitigation::NoGuest]);

/// What the kernel applies for a program that asked, on every CPU that
/// enumerates it ([`Decision::on_request`]). No register shows it at
/// bring-up, before any program has.
const ON_REQUEST: Mitigations = Mitigations::of(&[
    Mitigation::Ibpb,
    Mitigation::ReturnPredictorFilled,
    Mitigation::Stibp,
    Mitigation::Ssbd,
    Mitigation::VirtSsbd,
]);

/// What a CPU applies for a program that asked, and for no other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnRequest {
    /// `IA32_SPEC_CTRL` bits set while it runs: SSBD, and STIBP.
    pub spec_ctrl: u64,
    /// AMD's `VIRT_SPEC_CTRL` (C001_011FH) bits set while it runs: its SSBD
    /// (APM Vol. 3 Fn8000_0008_EBX[SsbdVirtSpecCtrl]).
    pub virt_spec_ctrl: u64,
    /// An IBPB on a switch between its address space and another's
    /// ([`LastAsked`]).
    pub ibpb: bool,
    /// The return address predictor filled after each such IBPB.
    pub fill_after_ibpb: bool,
}

/// Whether a program asked for what ToyOS applies only on request
/// ([`OnRequest`]). A program asks as the program that started it does, or
/// because its start asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speculation {
    #[default]
    Shared,
    Isolated,
}

impl Speculation {
    /// What a program this one starts runs with, where its start `asked` or not.
    pub const fn of_child(self, asked: bool) -> Self {
        if asked {
            Self::Isolated
        } else {
            Self::Shared
        }
    }
}

/// The address space a CPU last ran a program in, by its process's pid,
/// where that program asked. An IBPB goes between two spaces one of which
/// asked: ToyOS's policy on the barrier the vendors prescribe between
/// programs (Intel "Speculative Execution Side Channel Mitigations" §2.4.3;
/// AMD 70586 V2-4). The idle context runs no program's code and changes
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastAsked(Option<Pid>);

impl LastAsked {
    pub const fn new(space: Option<Pid>) -> Self {
        Self(space)
    }

    /// The space, where it asked.
    pub const fn space(self) -> Option<Pid> {
        self.0
    }

    /// A switch to `next`, a program's space and its ask, or `None` for the
    /// idle context: whether an IBPB goes before it runs.
    pub fn switch_to(&mut self, next: Option<(Pid, Speculation)>) -> bool {
        let Some((space, speculation)) = next else { return false };
        let asked = speculation == Speculation::Isolated;
        // A space that did not ask leaves none named, so an asking one owes
        // the barrier on arrival from any other.
        let barrier = match self.0 {
            Some(last) => last != space,
            None => asked,
        };
        self.0 = asked.then_some(space);
        barrier
    }
}

/// The registers that show which of a CPU's controls are on, each read only
/// where the decision found its control enumerated (an absent MSR is `#GP`)
/// and `None` elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registers {
    /// `IA32_SPEC_CTRL` (48H), wherever the CPU has it ([`Facts::has_spec_ctrl`]).
    pub spec_ctrl: Option<u64>,
    /// `IA32_MCU_OPT_CTRL` (123H).
    pub mcu_opt_ctrl: Option<u64>,
    /// AMD's `DE_CFG2` (C001_10E3H).
    pub de_cfg2: Option<u64>,
    /// AMD's MSR C001_1029, once the kernel has set what it sets there.
    pub c001_1029: Option<u64>,
}

/// MSR C001_1029[1]: LFENCE is dispatch serializing (AMD 70586 G-2).
pub const C001_1029_LFENCE_SERIALIZES_DISPATCH: u64 = 1 << 1;

/// The register bits the decision sets and reads: `IA32_SPEC_CTRL`'s
/// ("CPUID Enumeration and Architectural MSRs", Table 3, which AMD's
/// `SPEC_CTRL` shares: APM Vol. 3 Fn8000_0008_EBX), `IA32_MCU_OPT_CTRL`'s two
/// opt-outs (the same, Table 6) and `DE_CFG2[SuppressBPOnNonBr]` ("Technical
/// Guidance for Mitigating Branch Type Confusion" §6.2.1).
mod msr {
    pub const IBRS: u64 = 1 << 0;
    pub const STIBP: u64 = 1 << 1;
    /// Bit 2 of `IA32_SPEC_CTRL` and of AMD's `VIRT_SPEC_CTRL` alike.
    pub const SSBD: u64 = 1 << 2;
    pub const BHI_DIS_S: u64 = 1 << 10;
    pub const RNGDS_MITG_DIS: u64 = 1 << 0;
    pub const GDS_MITG_DIS: u64 = 1 << 4;
    pub const SUPPRESS_BP_ON_NON_BR: u64 = 1 << 1;
}

/// One vulnerability's line: `not affected`, `mitigated: ` and what by, or
/// `vulnerable: needs ` and what is missing; then `; SMT vulnerable: ` where
/// the kernel stays exposed to the core's other thread.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    verdict: Verdict,
    applied: Mitigations,
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (needs, lacks, by_policy, smt, assumed) = match self.verdict {
            Verdict::NotAffected => return f.write_str("not affected"),
            Verdict::Unknown => return f.write_str("unknown: no guidance is read for this CPU's vendor"),
            Verdict::Affected { needs, lacks, by_policy, smt, assumed } => (needs, lacks, by_policy, smt, assumed),
        };
        let named = |set| Named { set, by_policy };
        let missing = needs.without(self.applied);
        if missing.is_empty() {
            write!(f, "mitigated: {}", named(needs))?;
        } else {
            write!(f, "vulnerable: needs {}", named(missing))?;
            let has = needs.and(self.applied);
            if !has.is_empty() {
                write!(f, "; has {}", named(has))?;
            }
            if !lacks.is_empty() {
                write!(f, "; this CPU enumerates no {}", named(lacks))?;
            }
        }
        match smt {
            SmtExposure::None => {}
            SmtExposure::StibpOnRequest => f.write_str("; SMT vulnerable: STIBP only for programs that ask (policy)")?,
            SmtExposure::NoStibp => f.write_str("; SMT vulnerable: this CPU has no STIBP")?,
        }
        if assumed {
            f.write_str(" (no vendor row names this CPU: assumed affected)")?;
        }
        Ok(())
    }
}

/// A set's names, each marked where ToyOS's policy is what applies it.
struct Named {
    set: Mitigations,
    by_policy: Mitigations,
}

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for one in Mitigation::ALL.into_iter().filter(|&one| self.set.has(one)) {
            if !first {
                f.write_str(", ")?;
            }
            f.write_str(one.name())?;
            if self.by_policy.has(one) {
                f.write_str(" (policy)")?;
            }
            first = false;
        }
        Ok(())
    }
}

/// What an affected CPU is asked for, and which of it the CPU enumerates.
#[derive(Clone, Copy)]
struct Asks {
    needs: Mitigations,
    lacks: Mitigations,
    by_policy: Mitigations,
    smt: SmtExposure,
}

impl Asks {
    const NOTHING: Self = Self {
        needs: Mitigations::NONE,
        lacks: Mitigations::NONE,
        by_policy: Mitigations::NONE,
        smt: SmtExposure::None,
    };

    const fn of(one: Mitigation, enumerated: bool) -> Self {
        Self::NOTHING.and(one, enumerated)
    }

    const fn and(self, one: Mitigation, enumerated: bool) -> Self {
        Self {
            needs: self.needs.with(one),
            lacks: if enumerated { self.lacks } else { self.lacks.with(one) },
            ..self
        }
    }

    /// The kernel against the core's other thread, where SMT is on: the
    /// vendor asks `STIBP` of every program, and ToyOS's policy sets it only
    /// for a program that asks.
    const fn exposed_to_smt(self, smt: bool, stibp: bool) -> Self {
        let smt = match (smt, stibp) {
            (false, _) => SmtExposure::None,
            (true, true) => SmtExposure::StibpOnRequest,
            (true, false) => SmtExposure::NoStibp,
        };
        Self { smt, ..self }
    }

    /// `one`, which the vendor prescribes for every program, as ToyOS's
    /// policy applies it: for a program that asks.
    const fn by_policy(self, asked: bool, one: Mitigation, enumerated: bool) -> Self {
        if asked {
            let and = self.and(one, enumerated);
            Self { by_policy: and.by_policy.with(one), ..and }
        } else {
            self
        }
    }

    const fn and_if(self, asked: bool, one: Mitigation, enumerated: bool) -> Self {
        if asked {
            self.and(one, enumerated)
        } else {
            self
        }
    }

    const fn affected(self) -> Verdict {
        self.assumed(false)
    }

    const fn assumed(self, assumed: bool) -> Verdict {
        Verdict::Affected { needs: self.needs, lacks: self.lacks, by_policy: self.by_policy, smt: self.smt, assumed }
    }
}

pub fn decide(facts: &Facts) -> Decision {
    let verdict: fn(&Facts, Vulnerability) -> Verdict = match facts.vendor {
        Vendor::Intel => intel,
        Vendor::Amd => amd,
        Vendor::Other(_) => |_, _| Verdict::Unknown,
    };
    Decision(Vulnerability::ALL.map(|of| verdict(facts, of)))
}

impl Facts {
    /// Whether the CPU has `IA32_SPEC_CTRL` (48H), which an absent MSR's
    /// `#GP` makes a question to ask first: on Intel wherever it enumerates
    /// any of its bits ("CPUID Enumeration and Architectural MSRs", Table 3),
    /// on AMD wherever it enumerates IBRS, STIBP, SSBD or PSFD (APM Vol. 3
    /// Fn8000_0008_EBX).
    pub fn has_spec_ctrl(&self) -> bool {
        match self.vendor {
            Vendor::Intel => {
                self.cpuid_7_0_edx & (IBRS_IBPB | STIBP | SSBD) != 0
                    || self.cpuid_7_2_edx & (PSFD | IPRED_CTRL | RRSBA_CTRL | DDPD_U | BHI_CTRL) != 0
            }
            Vendor::Amd => self.cpuid_8000_0008_ebx & (amd::IBRS | amd::STIBP | amd::SSBD | amd::PSFD) != 0,
            Vendor::Other(_) => false,
        }
    }

    fn cap(&self, bit: u64) -> bool {
        self.arch_capabilities & bit != 0
    }

    /// Whether Intel's table lists this CPU as affected by `column`: `None`
    /// where it has no row for the CPU.
    fn intel_lists(&self, column: u16) -> Option<bool> {
        INTEL_ROWS.iter().find(|row| self.signature & row.mask == row.signature).map(|row| row.affected & column != 0)
    }

    /// The module's rule for a vulnerability Intel's table decides.
    fn intel_listed(&self, column: u16, asks: Asks) -> Verdict {
        match self.intel_lists(column) {
            Some(false) => Verdict::NotAffected,
            Some(true) => asks.affected(),
            None => asks.assumed(true),
        }
    }

    /// "Microarchitectural Data Sampling": `MDS_NO`, else the table; VERW
    /// where `MD_CLEAR`.
    fn intel_mds(&self) -> Verdict {
        if self.cap(cap::MDS_NO) {
            Verdict::NotAffected
        } else {
            self.intel_listed(MDS, Asks::of(Mitigation::Verw, self.cpuid_7_0_edx & MD_CLEAR != 0))
        }
    }

    /// A return on an empty RSB takes another predictor's target: "RSBA
    /// behavior is enumerated through CPUID family/model information or
    /// `IA32_ARCH_CAPABILITIES[RSBA]`", and a listed part that enumerates
    /// `IBRS_ALL` and not `RSBA` does not have it ("Retpoline", Table 5 and
    /// its note).
    fn intel_rsba(&self) -> bool {
        self.cap(cap::RSBA) || !self.cap(cap::IBRS_ALL) && INTEL_RSBA.contains(&self.signature)
    }
}

fn intel(f: &Facts, of: Vulnerability) -> Verdict {
    let edx = f.cpuid_7_0_edx;
    let ibrs = edx & IBRS_IBPB != 0;
    match of {
        // Every row of the table is affected.
        Vulnerability::SpectreV1 => Asks::of(Mitigation::UserPointerClamp, true).affected(),
        // "Speculative Execution Side Channel Mitigations": enhanced IBRS
        // where enumerated, with SMEP for the RSB. "Return Stack Buffer
        // Underflow": IBRS, not retpoline, where RSBA. Else retpoline, with
        // SMEP in place of an RSB overwrite on entry from user mode. STIBP is
        // for a process that wants its branches kept from its core's other
        // thread (§2.4.2), and enhanced IBRS keeps them already: "there is
        // no need to use STIBP when IBRS is set" (§2.4.1.3).
        Vulnerability::SpectreV2 => {
            let enhanced = f.cap(cap::IBRS_ALL);
            // The bit it sets is `IBRS`'s, which CPUID.(07H,0):EDX[26]
            // enumerates ("CPUID Enumeration and Architectural MSRs", Table 3).
            let branches = if enhanced {
                Asks::of(Mitigation::EnhancedIbrs, ibrs)
            } else if f.intel_rsba() {
                Asks::of(Mitigation::IbrsOnEntry, ibrs)
            } else {
                Asks::of(Mitigation::Retpoline, true)
            };
            branches
                .and(Mitigation::Smep, true)
                .and(Mitigation::Ibpb, ibrs)
                .and_if(f.smt && !enhanced, Mitigation::Stibp, edx & STIBP != 0)
                .affected()
        }
        // "Branch History Injection and Intra-mode Branch Target Injection",
        // its guidance for operating systems.
        Vulnerability::BranchHistoryInjection => {
            if f.cap(cap::BHI_NO) || f.intel_lists(BHI) == Some(false) {
                Verdict::NotAffected
            } else if f.cpuid_7_2_edx & BHI_CTRL != 0 {
                Asks::of(Mitigation::BhiDisS, true).affected()
            } else if f.cap(cap::IBRS_ALL) {
                Asks::of(Mitigation::BhbClear, true).affected()
            } else if f.hypervisor && ibrs && (f.intel_rsba() || f.cap(cap::RRSBA)) {
                // A guest that enumerates IBRS and not IBRS_ALL may run on
                // hardware that has it: the sequence, where the guest relies
                // on IBRS or its returns can underflow.
                Asks::of(Mitigation::BhbClear, true).affected()
            } else {
                Verdict::NotAffected
            }
        }
        // "Return Stack Buffer Underflow": a part with RRSBA_DIS_S is not
        // affected; enhanced IBRS holds RRSBA; IBRS holds RSBA.
        Vulnerability::Retbleed => {
            if f.cpuid_7_2_edx & RRSBA_CTRL != 0 {
                Verdict::NotAffected
            } else if f.cap(cap::IBRS_ALL) {
                if f.cap(cap::RRSBA) || f.intel_lists(RRSBA) == Some(true) {
                    Asks::of(Mitigation::EnhancedIbrs, ibrs).affected()
                } else {
                    Verdict::NotAffected
                }
            } else if f.intel_rsba() {
                Asks::of(Mitigation::IbrsOnEntry, ibrs).affected()
            } else {
                Verdict::NotAffected
            }
        }
        // "Indirect Target Selection", its algorithm: a guest by ITS_NO and
        // BHI_CTRL, a host by IBRS_ALL and the table.
        Vulnerability::IndirectTargetSelection => {
            let is = if f.hypervisor {
                !f.cap(cap::ITS_NO) && f.cpuid_7_2_edx & BHI_CTRL == 0
            } else {
                f.cap(cap::IBRS_ALL) && f.intel_lists(ITS) == Some(true)
            };
            if is {
                Asks::of(Mitigation::BranchesInUpperHalf, true).affected()
            } else {
                Verdict::NotAffected
            }
        }
        // "Speculative Store Bypass": `SSB_NO`; every row is affected.
        Vulnerability::SpecStoreBypass => {
            if f.cap(cap::SSB_NO) {
                Verdict::NotAffected
            } else {
                Asks::of(Mitigation::Ssbd, edx & SSBD != 0).affected()
            }
        }
        // "Rogue Data Cache Load": `RDCL_NO`, else the table.
        Vulnerability::Meltdown => {
            if f.cap(cap::RDCL_NO) {
                Verdict::NotAffected
            } else {
                f.intel_listed(MELTDOWN, Asks::of(Mitigation::KernelUnmappedInUserMode, true))
            }
        }
        // "Intel Analysis of L1 Terminal Fault": a processor with `RDCL_NO`
        // is not susceptible.
        Vulnerability::L1tf => {
            if f.cap(cap::RDCL_NO) {
                Verdict::NotAffected
            } else {
                f.intel_listed(L1TF, Asks::of(Mitigation::TerminalEntriesNameNoSecret, true))
            }
        }
        Vulnerability::Mds => f.intel_mds(),
        // "TSX Asynchronous Abort": affected if and only if the CPU has TSX
        // and does not enumerate `TAA_NO`; MDS's mitigation where MDS
        // affects it, else RTM off where `TSX_CTRL`, else VERW.
        Vulnerability::TsxAsyncAbort => {
            if f.cpuid_7_0_ebx & RTM == 0 || f.cap(cap::TAA_NO) {
                Verdict::NotAffected
            } else if f.intel_mds() == Verdict::NotAffected && f.cap(cap::TSX_CTRL) {
                Asks::of(Mitigation::RtmDisabled, true).affected()
            } else {
                Asks::of(Mitigation::Verw, edx & MD_CLEAR != 0).affected()
            }
        }
        // "Processor MMIO Stale Data Vulnerabilities": the three `_NO`
        // bits, else the table. Each propagator the part has asks its own:
        // - the fill buffer propagator, VERW's fill-buffer overwrite, where
        //   `FB_CLEAR` enumerates it or MDS affects the part and it has
        //   `MD_CLEAR` and `L1D_FLUSH`. The article's selection pseudocode
        //   asks a microcode update of every affected part without
        //   `FB_CLEAR`; its "Fill Buffer Clearing Operations" gives `FB_CLEAR`
        //   only to "processors affected by FBSDP", which this follows.
        // - Shared Buffers Data Read and the sideband propagator, the
        //   microcode that has RDRAND, RDSEED and EGETKEY overwrite the
        //   uncore buffers, which "is also relevant", under the control
        //   `SRBDS_CTRL` enumerates.
        // - the primary propagator, a read after each secret one, unless
        //   nothing can read the fill buffers it fills: `PSDP_NO`'s paragraph
        //   spares parts that "enumerate MDS_NO, FBSDP_NO, and either
        //   enumerate TAA_NO or do not support Intel TSX".
        Vulnerability::MmioStaleData => {
            if f.cap(cap::SBDR_SSDP_NO) && f.cap(cap::FBSDP_NO) && f.cap(cap::PSDP_NO) {
                return Verdict::NotAffected;
            }
            let implicit = f.intel_mds() != Verdict::NotAffected && edx & MD_CLEAR != 0 && edx & L1D_FLUSH != 0;
            let tsx = f.cpuid_7_0_ebx & (HLE | RTM) != 0;
            let fill_buffers_unread = f.cap(cap::MDS_NO) && f.cap(cap::FBSDP_NO) && (f.cap(cap::TAA_NO) || !tsx);
            let asks = Asks::NOTHING
                .and_if(!f.cap(cap::FBSDP_NO), Mitigation::Verw, f.cap(cap::FB_CLEAR) || implicit)
                .and_if(!f.cap(cap::SBDR_SSDP_NO), Mitigation::SrbdsMicrocode, edx & SRBDS_CTRL != 0)
                .and_if(!f.cap(cap::PSDP_NO) && !fill_buffers_unread, Mitigation::ReadAfterSecret, true);
            // Only the primary propagator, and nothing reads what it leaves.
            if asks.needs.is_empty() {
                return Verdict::NotAffected;
            }
            f.intel_listed(MMIO, asks)
        }
        // "Register File Data Sampling": `RFDS_NO`; an affected part whose
        // VERW clears the register files enumerates `RFDS_CLEAR`.
        Vulnerability::RegFileDataSampling => {
            if f.cap(cap::RFDS_NO) {
                Verdict::NotAffected
            } else if f.cap(cap::RFDS_CLEAR) {
                Asks::of(Mitigation::Verw, true).affected()
            } else {
                f.intel_listed(RFDS, Asks::of(Mitigation::Verw, false))
            }
        }
        // "Special Register Buffer Data Sampling": the table; the microcode
        // that mitigates it enumerates `SRBDS_CTRL`.
        Vulnerability::Srbds => f.intel_listed(SRBDS, Asks::of(Mitigation::SrbdsMicrocode, edx & SRBDS_CTRL != 0)),
        // "Gather Data Sampling": `GDS_NO`, else the table; the microcode
        // that mitigates it enumerates `GDS_CTRL`.
        Vulnerability::GatherDataSampling => {
            if f.cap(cap::GDS_NO) {
                Verdict::NotAffected
            } else {
                f.intel_listed(GDS, Asks::of(Mitigation::GdsMicrocode, f.cap(cap::GDS_CTRL)))
            }
        }
        // "Machine Check Error Avoidance on Page Size Change": a guest's
        // attack on its host, through pages the host maps for it.
        Vulnerability::ItlbMultihit => {
            if f.cap(cap::IF_PSCHANGE_MC_NO) {
                Verdict::NotAffected
            } else {
                f.intel_listed(MCEPSC, Asks::of(Mitigation::NoGuest, true))
            }
        }
        Vulnerability::SpecRstackOverflow | Vulnerability::Tsa => Verdict::NotAffected,
    }
}

/// "Zen 2", as AMD's Branch Type Confusion guidance and SRSO update give
/// its models: Family 17h Models 30h-4Fh, 60h-7Fh and A0h-AFh.
const fn zen2(id: &Ident) -> bool {
    id.family == 0x17 && matches!(id.model, 0x30..=0x4f | 0x60..=0x7f | 0xa0..=0xaf)
}

fn amd(f: &Facts, of: Vulnerability) -> Verdict {
    let ebx = f.cpuid_8000_0008_ebx;
    let eax = f.cpuid_8000_0021_eax;
    let id = Ident::new(f.vendor, f.signature);
    let ibpb = ebx & amd::IBPB != 0;
    let stibp = ebx & amd::STIBP != 0;
    // AMD-SB-1040: an IBPB may leave the return address predictor's entries,
    // and "in addition to performing the IBPB" AMD asks 70586's V2-3 fill,
    // which follows an IBPB and so is had only with one; `IBPB_RET`
    // enumerates an IBPB that clears them (APM Vol. 3 Fn8000_0008_EBX[30]).
    let fill = ebx & amd::IBPB_RET == 0;
    match of {
        // 70586 V1-2, and G-2 for LFENCE: its families 0Fh and 11h always
        // serialize and have no MSR; 10h, 12h to 17h and "all future
        // processors" have it, unless APM Vol. 3's LFENCE says CPUID makes it
        // always so. A guest cannot write its host's MSR.
        Vulnerability::SpectreV1 => Asks::of(Mitigation::UserPointerClamp, true)
            .and_if(
                eax & amd::LFENCE_ALWAYS_SERIALIZING == 0 && !matches!(id.family, 0x0f | 0x11),
                Mitigation::LfenceDispatchSerializing,
                !f.hypervisor,
            )
            .affected(),
        // 70586 V2-1, V2-4 and G-3, with the CPUID words' own preference
        // for IBRS (APM Vol. 3 Fn8000_0008_EBX, Fn8000_0021_EAX). Branch
        // Type Confusion §5 keeps the return address predictor safe with "a
        // RAP stuffing sequence (mitigation V2-3 …) and/or SMEP": SMEP, which
        // the kernel holds on every CPU, keeps Ring 0 from fetching the user
        // addresses user code's calls leave there (G-3).
        Vulnerability::SpectreV2 => {
            let branches = if eax & amd::AUTOMATIC_IBRS != 0 {
                Mitigation::AutomaticIbrs
            } else if ebx & amd::IBRS != 0 && ebx & amd::IBRS_ALWAYS_ON != 0 && ebx & amd::IBRS_PREFERRED != 0 {
                Mitigation::IbrsAlwaysOn
            } else {
                Mitigation::Retpoline
            };
            Asks::of(branches, true)
                .and(Mitigation::Smep, true)
                .and(Mitigation::Ibpb, ibpb)
                .and_if(fill, Mitigation::ReturnPredictorFilled, ibpb)
                .and_if(f.smt, Mitigation::Stibp, stibp)
                .affected()
        }
        // "Technical Guidance for Mitigating Branch Type Confusion":
        // `BTC_NO`; Family 19h is unaffected without it; Families 15h and
        // 17h are its table's, and it makes no statement on an older one.
        // Its §6.1.1 asks STIBP of every program for the kernel's sake: "If
        // the code on the sibling thread cannot be trusted, software should
        // set STIBP to 1 or disable SMT to ensure SMT safety when using
        // Jmp2Ret." Its appendix's footnotes 5 and 6: "Bulldozer", "Zen" and
        // "Zen+" have no STIBP.
        Vulnerability::Retbleed => {
            if ebx & amd::BTC_NO != 0 || id.family == 0x19 {
                return Verdict::NotAffected;
            }
            Asks::of(Mitigation::Jmp2Ret, true)
                // A guest cannot write its host's DE_CFG2.
                .and_if(zen2(&id) && !f.hypervisor, Mitigation::SuppressBpOnNonBr, true)
                .exposed_to_smt(f.smt, stibp)
                .assumed(!matches!(id.family, 0x15 | 0x17))
        }
        // "Technical Update Regarding Speculative Return Stack Overflow":
        // `SRSO_NO`; across user and kernel `SRSO_USER_KERNEL_NO`. Between
        // every two programs, its §3: "Mitigation of SRSO across user/user …
        // boundaries requires the use of IBPB unless SRSO_NO is 1".
        // AMD-SB-7005 names Zen to Zen 4, Families 17h and 19h; 70608
        // describes Safe-RET.
        Vulnerability::SpecRstackOverflow => {
            if eax & amd::SRSO_NO != 0 || id.family < 0x17 {
                return Verdict::NotAffected;
            }
            Asks::NOTHING
                .by_policy(true, Mitigation::Ibpb, ibpb)
                .by_policy(fill, Mitigation::ReturnPredictorFilled, ibpb)
                .and_if(eax & amd::SRSO_USER_KERNEL_NO == 0, Mitigation::SafeRet, true)
                .assumed(id.family > 0x19)
        }
        // APM Vol. 3 Fn8000_0008_EBX[26:24].
        Vulnerability::SpecStoreBypass => {
            if ebx & amd::SSBD_NOT_REQUIRED != 0 {
                Verdict::NotAffected
            } else if ebx & amd::SSBD == 0 && ebx & amd::SSBD_VIRT_SPEC_CTRL != 0 {
                Asks::of(Mitigation::VirtSsbd, true).affected()
            } else {
                Asks::of(Mitigation::Ssbd, ebx & amd::SSBD != 0).affected()
            }
        }
        // "Technical Guidance for Mitigating Transient Scheduler Attacks":
        // the two `_NO` bits; no family older than 19h; Family 19h, and
        // Family 1Ah Models 00h-4Fh and 60h-7Fh.
        Vulnerability::Tsa => {
            let ecx = f.cpuid_8000_0021_ecx;
            if ecx & amd::TSA_SQ_NO != 0 && ecx & amd::TSA_L1_NO != 0 || id.family < 0x19 {
                return Verdict::NotAffected;
            }
            // Family 19h never sets VERW_CLEAR itself: bare metal reads the
            // patch level, and a hypervisor synthesizes the bit.
            let patched = id.family == 0x19
                && !f.hypervisor
                && AMD_TSA_PATCH_LEVELS.iter().any(|&(model, stepping, level)| {
                    (model, stepping) == (id.model, id.stepping) && f.microcode >= Revision(level)
                });
            let named = id.family == 0x19 || id.family == 0x1a && matches!(id.model, 0x00..=0x4f | 0x60..=0x7f);
            Asks::of(Mitigation::Verw, eax & amd::VERW_CLEAR != 0 || patched).assumed(!named)
        }
        // 70586: software "should use CPUID vendor ID checks to identify
        // AMD processors to avoid implementing variant 3 mitigations". The
        // rest are Intel's advisories.
        Vulnerability::Meltdown
        | Vulnerability::BranchHistoryInjection
        | Vulnerability::IndirectTargetSelection
        | Vulnerability::L1tf
        | Vulnerability::Mds
        | Vulnerability::TsxAsyncAbort
        | Vulnerability::MmioStaleData
        | Vulnerability::RegFileDataSampling
        | Vulnerability::Srbds
        | Vulnerability::GatherDataSampling
        | Vulnerability::ItlbMultihit => Verdict::NotAffected,
    }
}

/// The patch level from which Family 19h's `VERW` mitigates TSA, by model
/// and stepping: the guidance's appendix, revision 3.0.
const AMD_TSA_PATCH_LEVELS: [(u32, u32, u32); 16] = [
    (0x01, 1, 0x0a00_11d7),
    (0x01, 2, 0x0a00_123b),
    (0x08, 2, 0x0a00_820d),
    (0x11, 1, 0x0a10_114c),
    (0x11, 2, 0x0a10_124c),
    (0x18, 1, 0x0a10_8109),
    (0x21, 0, 0x0a20_102e),
    (0x21, 2, 0x0a20_1211),
    (0x44, 1, 0x0a40_4108),
    (0x50, 0, 0x0a50_0012),
    (0x61, 2, 0x0a60_120a),
    (0x74, 1, 0x0a70_4108),
    (0x75, 2, 0x0a70_5208),
    (0x78, 0, 0x0a70_8008),
    (0x7c, 0, 0x0a70_c008),
    (0xa0, 2, 0x0aa0_0216),
];

/// "Retpoline: A Branch Target Injection Mitigation", Table 5: the
/// signatures with RSBA behavior that may not enumerate it.
const INTEL_RSBA: [u32; 12] =
    [0x406e3, 0x506e3, 0x50653, 0x50654, 0x60663, 0x806e9, 0x806ea, 0x806eb, 0x906e9, 0x906ea, 0x906eb, 0x906ec];

/// One CPU of Intel's table and the columns that list it as affected.
struct Row {
    signature: u32,
    /// `!0xf` where the row is every stepping of its model.
    mask: u32,
    affected: u16,
}

const fn row(signature: u32, affected: u16) -> Row {
    Row { signature, mask: !0, affected }
}

const fn any_stepping(signature: u32, affected: u16) -> Row {
    Row { signature, mask: !0xf, affected }
}

// The table's columns: Rogue Data Cache Load; L1 Terminal Fault; the three
// Microarchitectural Data Sampling columns; Machine
// Check Error Avoidance (CVE-2018-12207); Special Register Buffer Data
// Sampling; the two MMIO columns; Gather Data Sampling; Register File Data
// Sampling; the three Indirect Target Selection columns; RSBU's RRSBA
// column; Branch History Injection.
const MELTDOWN: u16 = 1 << 0;
const L1TF: u16 = 1 << 1;
const MDS: u16 = 1 << 2;
const MCEPSC: u16 = 1 << 3;
const SRBDS: u16 = 1 << 4;
const MMIO: u16 = 1 << 5;
const GDS: u16 = 1 << 6;
const RFDS: u16 = 1 << 7;
const ITS: u16 = 1 << 8;
const RRSBA: u16 = 1 << 9;
const BHI: u16 = 1 << 10;

/// Intel's "Affected Processors: Guidance for Security Issues on Intel
/// Processors" as its machine-readable list gave it on 2026-09-04: each
/// CPUID it has a row for, with a column set where any row of that CPUID
/// says other than "Not Affected". Two rows of one CPUID differ only by
/// market segment or core mix, which the CPU's words do not tell apart.
#[rustfmt::skip]
const INTEL_ROWS: [Row; 67] = [
    row(0x306f2, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x306f4, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x406f1, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x50653, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x50654, MELTDOWN | L1TF | MDS | MCEPSC | MMIO | GDS),
    row(0x5065b, MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0x50663, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x50664, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x50665, MELTDOWN | L1TF | MDS | MCEPSC | MMIO),
    row(0x506ca, RFDS),
    row(0x506e3, MELTDOWN | L1TF | MDS | MCEPSC | SRBDS | MMIO | GDS),
    any_stepping(0x506f0, RFDS),
    row(0x606a6, MMIO | GDS | ITS | BHI),
    any_stepping(0x606c0, MMIO | GDS | ITS | BHI),
    row(0x706a8, RFDS | BHI),
    row(0x706e5, MDS | MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0x80665, MMIO | RFDS | BHI),
    row(0x80667, MMIO | RFDS | BHI),
    row(0x806c1, MMIO | GDS | ITS | BHI),
    row(0x806c2, GDS | ITS | BHI),
    row(0x806d1, MMIO | GDS | ITS | BHI),
    row(0x806e9, MELTDOWN | L1TF | MDS | MCEPSC | SRBDS | MMIO | GDS),
    row(0x806ea, MELTDOWN | L1TF | MDS | MCEPSC | SRBDS | MMIO | GDS),
    row(0x806eb, MDS | MCEPSC | SRBDS | MMIO | GDS),
    row(0x806ec, MCEPSC | SRBDS | MMIO | GDS | ITS | RRSBA | BHI),
    row(0x806f5, RRSBA | BHI),
    row(0x806f6, RRSBA | BHI),
    row(0x806f7, RRSBA | BHI),
    row(0x806f8, RRSBA | BHI),
    any_stepping(0x90660, MMIO | RFDS | BHI),
    row(0x90672, RFDS | BHI),
    row(0x90675, RFDS | BHI),
    row(0x906a3, RFDS | BHI),
    row(0x906a4, RFDS | BHI),
    any_stepping(0x906c0, MMIO | RFDS | BHI),
    row(0x906e9, MELTDOWN | L1TF | MDS | MCEPSC | SRBDS | MMIO | GDS),
    row(0x906ea, MELTDOWN | L1TF | MDS | MCEPSC | SRBDS | MMIO | GDS),
    row(0x906ec, MDS | MCEPSC | SRBDS | MMIO | GDS),
    row(0x906ed, MCEPSC | SRBDS | MMIO | GDS | ITS | RRSBA | BHI),
    row(0xa0652, MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0xa0653, MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0xa0655, MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0xa0660, MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0xa0661, MCEPSC | MMIO | GDS | ITS | RRSBA | BHI),
    row(0xa0671, MMIO | GDS | ITS | BHI),
    row(0xa06a4, RRSBA | BHI),
    row(0xa06d1, RRSBA | BHI),
    row(0xa06e1, RRSBA | BHI),
    row(0xa06f3, RRSBA | BHI),
    row(0xb0650, RRSBA | BHI),
    row(0xb0664, RRSBA | BHI),
    row(0xb0671, RFDS | RRSBA | BHI),
    row(0xb06a2, RFDS | RRSBA | BHI),
    row(0xb06a3, RFDS | RRSBA | BHI),
    row(0xb06a8, RFDS | RRSBA | BHI),
    row(0xb06d1, RRSBA | BHI),
    row(0xb06e0, RFDS | BHI),
    row(0xb06f2, RFDS | RRSBA | BHI),
    row(0xb06f5, RFDS | RRSBA | BHI),
    row(0xc0652, RRSBA | BHI),
    row(0xc0662, RRSBA | BHI),
    row(0xc06c2, RRSBA | BHI),
    row(0xc06c3, RRSBA | BHI),
    row(0xc06f2, RRSBA | BHI),
    row(0xd0651, RRSBA | BHI),
    row(0xd0670, RRSBA | BHI),
    row(0xd06d1, RRSBA | BHI),
];

#[cfg(test)]
mod tests {
    use super::Mitigation::*;
    use super::Vulnerability::*;
    use super::*;

    const fn needs(all: &[Mitigation]) -> Verdict {
        lacking(all, &[])
    }

    const fn lacking(all: &[Mitigation], lacks: &[Mitigation]) -> Verdict {
        Verdict::Affected {
            needs: Mitigations::of(all),
            lacks: Mitigations::of(lacks),
            by_policy: Mitigations::NONE,
            smt: SmtExposure::None,
            assumed: false,
        }
    }

    fn assumed(verdict: Verdict) -> Verdict {
        match verdict {
            Verdict::Affected { needs, lacks, by_policy, smt, .. } => Verdict::Affected { needs, lacks, by_policy, smt, assumed: true },
            other => other,
        }
    }

    /// `verdict` with `by_policy` of its needs ToyOS's policy.
    fn policy(verdict: Verdict, by_policy: &[Mitigation]) -> Verdict {
        match verdict {
            Verdict::Affected { needs, lacks, smt, assumed, .. } => {
                Verdict::Affected { needs, lacks, by_policy: Mitigations::of(by_policy), smt, assumed }
            }
            other => other,
        }
    }

    /// `verdict` leaving the kernel exposed to the core's other thread.
    fn exposed(verdict: Verdict, smt: SmtExposure) -> Verdict {
        match verdict {
            Verdict::Affected { needs, lacks, by_policy, assumed, .. } => Verdict::Affected { needs, lacks, by_policy, smt, assumed },
            other => other,
        }
    }

    const NOTHING: Facts = Facts {
        vendor: Vendor::Intel,
        signature: 0,
        hypervisor: false,
        smt: false,
        microcode: Revision(0),
        cpuid_7_0_ebx: 0,
        cpuid_7_0_edx: 0,
        cpuid_7_2_edx: 0,
        arch_capabilities: 0,
        cpuid_8000_0008_ebx: 0,
        cpuid_8000_0021_eax: 0,
        cpuid_8000_0021_ecx: 0,
    };

    /// Holds `decide(facts)` to `affected` and to "not affected" for every
    /// vulnerability `affected` leaves out.
    #[track_caller]
    fn holds(facts: &Facts, affected: &[(Vulnerability, Verdict)]) {
        let decision = decide(facts);
        for of in Vulnerability::ALL {
            let want = affected.iter().find(|(named, _)| *named == of).map_or(Verdict::NotAffected, |(_, verdict)| *verdict);
            assert_eq!(decision.verdict(of), want, "{}", of.name());
        }
    }

    /// Intel's row for CPUID 806C1, eight threads on four cores, with the
    /// bits its rules read of what the T14 enumerates on its listed
    /// microcode: enhanced IBRS; `FBSDP_NO` and no `FB_CLEAR`, so that its
    /// "MCU" for Shared Buffers Data Read is the uncore buffers' overwrite
    /// `SRBDS_CTRL` controls; `GDS_CTRL` for its gather "MCU"; and neither
    /// `BHI_NO` nor `BHI_CTRL` for its "Software".
    const INTEL_806C1: Facts = Facts {
        signature: 0x806c1,
        smt: true,
        cpuid_7_0_edx: SRBDS_CTRL | MD_CLEAR | IBRS_IBPB | STIBP | L1D_FLUSH | CPUID_7_0_EDX_ARCH_CAPABILITIES | SSBD,
        arch_capabilities: cap::RDCL_NO
            | cap::IBRS_ALL
            | cap::MDS_NO
            | cap::IF_PSCHANGE_MC_NO
            | cap::FBSDP_NO
            | cap::GDS_CTRL
            | cap::RFDS_NO,
        ..NOTHING
    };

    #[test]
    fn intels_row_for_806c1_decides_a_cpu_with_its_words() {
        holds(
            &INTEL_806C1,
            &[
                (SpectreV1, needs(&[UserPointerClamp])),
                (SpectreV2, needs(&[EnhancedIbrs, Smep, Ibpb])),
                (BranchHistoryInjection, needs(&[BhbClear])),
                (IndirectTargetSelection, needs(&[BranchesInUpperHalf])),
                (SpecStoreBypass, needs(&[Ssbd])),
                (MmioStaleData, needs(&[SrbdsMicrocode])),
                (GatherDataSampling, needs(&[GdsMicrocode])),
            ],
        );
        // A part with the fill buffer propagator asks VERW as well, which
        // only `FB_CLEAR` enumerates where MDS does not affect it.
        let propagating = Facts { arch_capabilities: INTEL_806C1.arch_capabilities & !cap::FBSDP_NO, ..INTEL_806C1 };
        assert_eq!(decide(&propagating).verdict(MmioStaleData), lacking(&[Verw, SrbdsMicrocode, ReadAfterSecret], &[Verw]));
        let cleared = Facts { arch_capabilities: propagating.arch_capabilities | cap::FB_CLEAR, ..propagating };
        assert_eq!(decide(&cleared).verdict(MmioStaleData), needs(&[Verw, SrbdsMicrocode, ReadAfterSecret]));
        // Without enhanced IBRS its core's other thread can steer a
        // program's branches, which STIBP keeps from a program that asks.
        let legacy = Facts { arch_capabilities: INTEL_806C1.arch_capabilities & !cap::IBRS_ALL, ..INTEL_806C1 };
        assert_eq!(decide(&legacy).verdict(SpectreV2), needs(&[Retpoline, Smep, Ibpb, Stibp]));
    }

    /// AMD's Family 17h Model 60h, "Zen 2", sixteen threads on eight cores:
    /// the Branch Type Confusion table's third column and AMD-SB-7005's
    /// "Zen 2", with the indirect branch controls and SSBD enumerated and
    /// IBRS not preferred.
    const AMD_17H_60H: Facts = Facts {
        vendor: Vendor::Amd,
        signature: 0x0086_0f01,
        smt: true,
        cpuid_8000_0008_ebx: amd::IBPB | amd::IBRS | amd::STIBP | amd::SSBD,
        ..NOTHING
    };

    #[test]
    fn amds_documents_for_zen_2_decide_a_family_17h_model_60h() {
        holds(
            &AMD_17H_60H,
            &[
                (SpectreV1, needs(&[UserPointerClamp, LfenceDispatchSerializing])),
                (SpectreV2, needs(&[Retpoline, Smep, Ibpb, ReturnPredictorFilled, Stibp])),
                (Retbleed, exposed(needs(&[Jmp2Ret, SuppressBpOnNonBr]), SmtExposure::StibpOnRequest)),
                (SpecRstackOverflow, policy(needs(&[SafeRet, Ibpb, ReturnPredictorFilled]), &[Ibpb, ReturnPredictorFilled])),
                (SpecStoreBypass, needs(&[Ssbd])),
            ],
        );
        // "Zen+", Family 17h Model 08h, has no STIBP and no DE_CFG2[1]:
        // only SMT off keeps Jmp2Ret from the core's other thread.
        let zen_plus = Facts { signature: 0x0080_0f82, cpuid_8000_0008_ebx: amd::IBPB | amd::SSBD, ..AMD_17H_60H };
        assert_eq!(decide(&zen_plus).verdict(Retbleed), exposed(needs(&[Jmp2Ret]), SmtExposure::NoStibp));
        // Its guest cannot write DE_CFG2, and a core with one thread is
        // exposed to no other.
        let guest = decide(&Facts { hypervisor: true, smt: false, ..AMD_17H_60H });
        assert_eq!(guest.verdict(Retbleed), needs(&[Jmp2Ret]));
        assert_eq!(
            guest.verdict(SpectreV1),
            lacking(&[UserPointerClamp, LfenceDispatchSerializing], &[LfenceDispatchSerializing])
        );
        assert_eq!(guest.verdict(SpectreV2), needs(&[Retpoline, Smep, Ibpb, ReturnPredictorFilled]));
        // An IBPB that clears the return address predictor needs no fill.
        let cleared = decide(&Facts { cpuid_8000_0008_ebx: AMD_17H_60H.cpuid_8000_0008_ebx | amd::IBPB_RET, ..AMD_17H_60H });
        assert_eq!(cleared.verdict(SpectreV2), needs(&[Retpoline, Smep, Ibpb, Stibp]));
        assert_eq!(cleared.verdict(SpecRstackOverflow), policy(needs(&[SafeRet, Ibpb]), &[Ibpb]));
    }

    /// CPUID 506E3 as it came, before any microcode gave it
    /// `IA32_ARCH_CAPABILITIES`: every `_NO` bit reads clear and the table
    /// decides, and "Retpoline"'s Table 5 lists it.
    #[test]
    fn a_cpu_without_arch_capabilities_is_decided_by_its_row() {
        let edx = SRBDS_CTRL | MD_CLEAR | IBRS_IBPB | STIBP | L1D_FLUSH | SSBD;
        holds(
            &Facts { signature: 0x506e3, smt: true, cpuid_7_0_ebx: RTM, cpuid_7_0_edx: edx, ..NOTHING },
            &[
                (SpectreV1, needs(&[UserPointerClamp])),
                (SpectreV2, needs(&[IbrsOnEntry, Smep, Ibpb, Stibp])),
                (Retbleed, needs(&[IbrsOnEntry])),
                (SpecStoreBypass, needs(&[Ssbd])),
                (Meltdown, needs(&[KernelUnmappedInUserMode])),
                (L1tf, needs(&[TerminalEntriesNameNoSecret])),
                (Mds, needs(&[Verw])),
                (TsxAsyncAbort, needs(&[Verw])),
                (MmioStaleData, needs(&[Verw, SrbdsMicrocode, ReadAfterSecret])),
                (Srbds, needs(&[SrbdsMicrocode])),
                (GatherDataSampling, lacking(&[GdsMicrocode], &[GdsMicrocode])),
                (ItlbMultihit, needs(&[NoGuest])),
            ],
        );
    }

    /// The same CPU with nothing enumerated: what it needs and cannot have.
    #[test]
    fn a_control_the_cpu_does_not_enumerate_is_lacking() {
        let decision = decide(&Facts { signature: 0x506e3, smt: true, ..NOTHING });
        assert_eq!(decision.verdict(SpectreV2), lacking(&[IbrsOnEntry, Smep, Ibpb, Stibp], &[IbrsOnEntry, Ibpb, Stibp]));
        assert_eq!(decision.verdict(SpecStoreBypass), lacking(&[Ssbd], &[Ssbd]));
        assert_eq!(decision.verdict(Mds), lacking(&[Verw], &[Verw]));
        assert_eq!(decision.verdict(MmioStaleData), lacking(&[Verw, SrbdsMicrocode, ReadAfterSecret], &[Verw, SrbdsMicrocode]));
        assert_eq!(decision.verdict(Srbds), lacking(&[SrbdsMicrocode], &[SrbdsMicrocode]));
    }

    /// CPUID 806F8, a part with `BHI_CTRL` and `RRSBA_CTRL`: `BHI_DIS_S`
    /// rather than the sequence, and no RSB underflow though it lists RRSBA.
    #[test]
    fn the_indirect_predictor_controls_decide_where_enumerated() {
        let facts = Facts {
            signature: 0x806f8,
            smt: true,
            cpuid_7_0_edx: IBRS_IBPB | STIBP | CPUID_7_0_EDX_ARCH_CAPABILITIES | SSBD,
            cpuid_7_2_edx: RRSBA_CTRL | BHI_CTRL,
            arch_capabilities: cap::RDCL_NO
                | cap::IBRS_ALL
                | cap::MDS_NO
                | cap::IF_PSCHANGE_MC_NO
                | cap::TAA_NO
                | cap::SBDR_SSDP_NO
                | cap::FBSDP_NO
                | cap::PSDP_NO
                | cap::RRSBA
                | cap::GDS_NO
                | cap::RFDS_NO,
            ..NOTHING
        };
        holds(
            &facts,
            &[
                (SpectreV1, needs(&[UserPointerClamp])),
                (SpectreV2, needs(&[EnhancedIbrs, Smep, Ibpb])),
                (BranchHistoryInjection, needs(&[BhiDisS])),
                (SpecStoreBypass, needs(&[Ssbd])),
            ],
        );
        // Without the controls enhanced IBRS holds its RRSBA, and the
        // sequence its branch history.
        let without = decide(&Facts { cpuid_7_2_edx: 0, ..facts });
        assert_eq!(without.verdict(Retbleed), needs(&[EnhancedIbrs]));
        assert_eq!(without.verdict(BranchHistoryInjection), needs(&[BhbClear]));
        assert_eq!(decide(&Facts { arch_capabilities: facts.arch_capabilities | cap::BHI_NO, ..facts }).verdict(BranchHistoryInjection), Verdict::NotAffected);
    }

    #[test]
    fn rfds_is_enumerated_by_rfds_clear_and_listed_without_it() {
        let listed = Facts { signature: 0xb06e0, ..NOTHING };
        assert_eq!(decide(&listed).verdict(RegFileDataSampling), lacking(&[Verw], &[Verw]));
        let cleared = Facts { arch_capabilities: cap::RFDS_CLEAR, ..listed };
        assert_eq!(decide(&cleared).verdict(RegFileDataSampling), needs(&[Verw]));
        let unaffected = Facts { arch_capabilities: cap::RFDS_NO, ..listed };
        assert_eq!(decide(&unaffected).verdict(RegFileDataSampling), Verdict::NotAffected);
        // A row for every stepping of its model: 06_5FH's.
        assert_eq!(decide(&Facts { signature: 0x506f1, ..NOTHING }).verdict(RegFileDataSampling), lacking(&[Verw], &[Verw]));
        assert_eq!(decide(&INTEL_806C1).verdict(RegFileDataSampling), Verdict::NotAffected);
    }

    /// A part whose only MMIO propagator is the primary one: unaffected
    /// where nothing reads the fill buffers it fills, as `PSDP_NO`'s
    /// paragraph says, and asked the read after a secret one where TSX
    /// without `TAA_NO` could.
    #[test]
    fn the_primary_propagator_alone_matters_where_fill_buffers_can_be_read() {
        let mmio = |facts: &Facts| decide(facts).verdict(MmioStaleData);
        let primary = Facts {
            arch_capabilities: cap::RDCL_NO | cap::IBRS_ALL | cap::MDS_NO | cap::SBDR_SSDP_NO | cap::FBSDP_NO,
            ..INTEL_806C1
        };
        assert_eq!(mmio(&primary), Verdict::NotAffected);
        assert_eq!(mmio(&Facts { arch_capabilities: primary.arch_capabilities & !cap::MDS_NO, ..primary }), needs(&[ReadAfterSecret]));
        let tsx = Facts { cpuid_7_0_ebx: RTM, ..primary };
        assert_eq!(mmio(&tsx), needs(&[ReadAfterSecret]));
        assert_eq!(mmio(&Facts { cpuid_7_0_ebx: HLE, ..primary }), needs(&[ReadAfterSecret]));
        assert_eq!(mmio(&Facts { arch_capabilities: tsx.arch_capabilities | cap::TAA_NO, ..tsx }), Verdict::NotAffected);
        assert_eq!(mmio(&Facts { arch_capabilities: tsx.arch_capabilities | cap::PSDP_NO, ..tsx }), Verdict::NotAffected);
    }

    /// TAA is decided by enumeration alone: RTM without `TAA_NO`. Where MDS
    /// does not affect the part, RTM goes if `TSX_CTRL` can turn it off.
    #[test]
    fn taa_follows_rtm_and_takes_the_control_it_has() {
        let part = Facts { cpuid_7_0_ebx: RTM, cpuid_7_0_edx: MD_CLEAR, arch_capabilities: cap::MDS_NO, ..INTEL_806C1 };
        assert_eq!(decide(&part).verdict(TsxAsyncAbort), needs(&[Verw]));
        let controlled = Facts { arch_capabilities: cap::MDS_NO | cap::TSX_CTRL, ..part };
        assert_eq!(decide(&controlled).verdict(TsxAsyncAbort), needs(&[RtmDisabled]));
        assert_eq!(decide(&Facts { cpuid_7_0_ebx: 0, ..part }).verdict(TsxAsyncAbort), Verdict::NotAffected);
        assert_eq!(decide(&Facts { arch_capabilities: cap::MDS_NO | cap::TAA_NO, ..part }).verdict(TsxAsyncAbort), Verdict::NotAffected);
    }

    /// CPUID 306A9, past its servicing life and in no row: assumed affected
    /// by what the table alone decides, and not by what enumeration or an
    /// algorithm of Intel's decides without it.
    #[test]
    fn an_intel_cpu_the_table_does_not_list_is_assumed_affected() {
        let edx = MD_CLEAR | IBRS_IBPB | STIBP | L1D_FLUSH | SSBD;
        holds(
            &Facts { signature: 0x306a9, cpuid_7_0_edx: edx, ..NOTHING },
            &[
                (SpectreV1, needs(&[UserPointerClamp])),
                (SpectreV2, needs(&[Retpoline, Smep, Ibpb])),
                (SpecStoreBypass, needs(&[Ssbd])),
                (Meltdown, assumed(needs(&[KernelUnmappedInUserMode]))),
                (L1tf, assumed(needs(&[TerminalEntriesNameNoSecret]))),
                (Mds, assumed(needs(&[Verw]))),
                (MmioStaleData, assumed(lacking(&[Verw, SrbdsMicrocode, ReadAfterSecret], &[SrbdsMicrocode]))),
                (RegFileDataSampling, assumed(lacking(&[Verw], &[Verw]))),
                (Srbds, assumed(lacking(&[SrbdsMicrocode], &[SrbdsMicrocode]))),
                (GatherDataSampling, assumed(lacking(&[GdsMicrocode], &[GdsMicrocode]))),
                (ItlbMultihit, assumed(needs(&[NoGuest]))),
            ],
        );
    }

    /// Intel's algorithm for ITS: a host by its row and `IBRS_ALL`, a guest
    /// by `ITS_NO` and `BHI_CTRL` whatever its signature.
    #[test]
    fn its_is_the_tables_on_a_host_and_the_hypervisors_in_a_guest() {
        let its = |facts: &Facts| decide(facts).verdict(IndirectTargetSelection);
        assert_eq!(its(&INTEL_806C1), needs(&[BranchesInUpperHalf]));
        assert_eq!(its(&Facts { arch_capabilities: 0, ..INTEL_806C1 }), Verdict::NotAffected);
        let guest = Facts { signature: 0x806f8, hypervisor: true, ..NOTHING };
        assert_eq!(its(&guest), needs(&[BranchesInUpperHalf]));
        assert_eq!(its(&Facts { arch_capabilities: cap::ITS_NO, ..guest }), Verdict::NotAffected);
        assert_eq!(its(&Facts { cpuid_7_2_edx: BHI_CTRL, ..guest }), Verdict::NotAffected);
        assert_eq!(its(&Facts { hypervisor: false, arch_capabilities: cap::IBRS_ALL, ..guest }), Verdict::NotAffected);
    }

    /// Family 19h Model 01h Stepping 1: unaffected by Branch Type Confusion
    /// without `BTC_NO`, and its VERW mitigates TSA from the appendix's
    /// patch level on bare metal, by `VERW_CLEAR` in a guest.
    #[test]
    fn family_19h_reads_its_patch_level_for_tsa() {
        let old = Facts {
            vendor: Vendor::Amd,
            signature: 0x00a0_0f11,
            smt: true,
            microcode: Revision(0x0a00_11d6),
            cpuid_8000_0008_ebx: amd::IBPB | amd::IBRS | amd::STIBP | amd::IBRS_ALWAYS_ON | amd::IBRS_PREFERRED | amd::SSBD,
            ..NOTHING
        };
        holds(
            &old,
            &[
                (SpectreV1, needs(&[UserPointerClamp, LfenceDispatchSerializing])),
                (SpectreV2, needs(&[IbrsAlwaysOn, Smep, Ibpb, ReturnPredictorFilled, Stibp])),
                (SpecRstackOverflow, policy(needs(&[SafeRet, Ibpb, ReturnPredictorFilled]), &[Ibpb, ReturnPredictorFilled])),
                (SpecStoreBypass, needs(&[Ssbd])),
                (Tsa, lacking(&[Verw], &[Verw])),
            ],
        );
        let tsa = |facts: &Facts| decide(facts).verdict(Tsa);
        let patched = Facts { microcode: Revision(0x0a00_11d7), ..old };
        assert_eq!(tsa(&patched), needs(&[Verw]));
        // Another stepping's level says nothing of this one.
        assert_eq!(tsa(&Facts { signature: 0x00a0_0f12, ..patched }), lacking(&[Verw], &[Verw]));
        assert_eq!(tsa(&Facts { hypervisor: true, ..patched }), lacking(&[Verw], &[Verw]));
        assert_eq!(tsa(&Facts { hypervisor: true, cpuid_8000_0021_eax: amd::VERW_CLEAR, ..old }), needs(&[Verw]));
        assert_eq!(tsa(&Facts { cpuid_8000_0021_ecx: amd::TSA_SQ_NO | amd::TSA_L1_NO, ..old }), Verdict::NotAffected);
    }

    /// A family newer than AMD's documents name is held to its `_NO` bits.
    #[test]
    fn a_newer_amd_family_is_decided_by_what_it_enumerates() {
        let facts = Facts {
            vendor: Vendor::Amd,
            signature: 0x00b0_0f00 | 0x8 << 16 | 0x4 << 4,
            cpuid_8000_0008_ebx: amd::IBPB | amd::STIBP | amd::SSBD | amd::BTC_NO | amd::IBPB_RET,
            cpuid_8000_0021_eax: amd::AUTOMATIC_IBRS | amd::SRSO_USER_KERNEL_NO,
            cpuid_8000_0021_ecx: amd::TSA_SQ_NO | amd::TSA_L1_NO,
            ..NOTHING
        };
        assert_eq!(Ident::new(facts.vendor, facts.signature).family, 0x1a);
        holds(
            &facts,
            &[
                (SpectreV1, needs(&[UserPointerClamp, LfenceDispatchSerializing])),
                (SpectreV2, needs(&[AutomaticIbrs, Smep, Ibpb])),
                (SpecRstackOverflow, assumed(policy(needs(&[Ibpb]), &[Ibpb]))),
                (SpecStoreBypass, needs(&[Ssbd])),
            ],
        );
        let bare = decide(&Facts { cpuid_8000_0008_ebx: 0, cpuid_8000_0021_eax: 0, cpuid_8000_0021_ecx: 0, ..facts });
        assert_eq!(bare.verdict(Retbleed), assumed(needs(&[Jmp2Ret])));
        let fill = ReturnPredictorFilled;
        assert_eq!(bare.verdict(SpecRstackOverflow), assumed(policy(lacking(&[SafeRet, Ibpb, fill], &[Ibpb, fill]), &[Ibpb, fill])));
        assert_eq!(bare.verdict(Tsa), assumed(lacking(&[Verw], &[Verw])));
        assert_eq!(bare.verdict(SpecStoreBypass), lacking(&[Ssbd], &[Ssbd]));
    }

    /// 70586 G-2's MSR wherever LFENCE does not already serialize dispatch:
    /// by CPUID, or in Families 0Fh and 11h.
    #[test]
    fn amds_lfence_serializes_dispatch_by_its_msr_unless_it_always_does() {
        let v1 = |facts: &Facts| decide(facts).verdict(SpectreV1);
        assert_eq!(v1(&AMD_17H_60H), needs(&[UserPointerClamp, LfenceDispatchSerializing]));
        let always = Facts { cpuid_8000_0021_eax: amd::LFENCE_ALWAYS_SERIALIZING, ..AMD_17H_60H };
        assert_eq!(v1(&always), needs(&[UserPointerClamp]));
        assert_eq!(v1(&Facts { hypervisor: true, ..always }), needs(&[UserPointerClamp]));
        for (family, needed) in [(0x0f, false), (0x10, true), (0x11, false), (0x16, true)] {
            let facts = Facts { signature: 0xf00 | (family - 0xf) << 20, ..AMD_17H_60H };
            assert_eq!(Ident::new(facts.vendor, facts.signature).family, family);
            let want = if needed { needs(&[UserPointerClamp, LfenceDispatchSerializing]) } else { needs(&[UserPointerClamp]) };
            assert_eq!(v1(&facts), want, "family {family:#x}");
        }
        // Intel's LFENCE holds back every later instruction by definition.
        assert_eq!(v1(&INTEL_806C1), needs(&[UserPointerClamp]));
    }

    #[test]
    fn amds_ssbd_is_the_control_it_enumerates() {
        let ssb = |ebx| decide(&Facts { cpuid_8000_0008_ebx: ebx, ..AMD_17H_60H }).verdict(SpecStoreBypass);
        assert_eq!(ssb(amd::SSBD | amd::SSBD_VIRT_SPEC_CTRL), needs(&[Ssbd]));
        assert_eq!(ssb(amd::SSBD_VIRT_SPEC_CTRL), needs(&[VirtSsbd]));
        assert_eq!(ssb(amd::SSBD_NOT_REQUIRED), Verdict::NotAffected);
    }

    #[test]
    fn another_vendors_cpu_is_unknown() {
        let decision = decide(&Facts { vendor: Vendor::Other(*b"HygonGenuine"), ..AMD_17H_60H });
        assert!(Vulnerability::ALL.iter().all(|&of| decision.verdict(of) == Verdict::Unknown));
    }

    #[test]
    fn a_line_says_what_is_in_force_and_what_is_missing() {
        let decision = decide(&AMD_17H_60H);
        let line = |of, applied: &[Mitigation]| std::format!("{}", decision.line(of, Mitigations::of(applied)));
        assert_eq!(line(Meltdown, &[]), "not affected");
        assert_eq!(
            line(SpectreV1, &[UserPointerClamp, LfenceDispatchSerializing, Smep]),
            "mitigated: user pointers clamped without a branch, LFENCE dispatch-serializing"
        );
        assert_eq!(
            line(SpectreV1, &[UserPointerClamp]),
            "vulnerable: needs LFENCE dispatch-serializing; has user pointers clamped without a branch"
        );
        assert_eq!(
            line(Retbleed, &[SuppressBpOnNonBr]),
            "vulnerable: needs Jmp2Ret; has DE_CFG2[SuppressBPOnNonBr]; SMT vulnerable: STIBP only for programs that ask (policy)"
        );
        // What the policy leaves exposed stays named once all else is in force.
        assert_eq!(
            line(Retbleed, &[Jmp2Ret, SuppressBpOnNonBr]),
            "mitigated: Jmp2Ret, DE_CFG2[SuppressBPOnNonBr]; SMT vulnerable: STIBP only for programs that ask (policy)"
        );
        assert_eq!(
            line(SpecRstackOverflow, &[]),
            "vulnerable: needs IBPB between programs that ask (policy), \
             return address predictor filled after IBPB (policy), Safe-RET"
        );
        // The vendor's own choice is not marked.
        assert_eq!(
            line(SpectreV2, &[Smep]),
            "vulnerable: needs retpoline, IBPB between programs that ask, \
             return address predictor filled after IBPB, STIBP for programs that ask; has SMEP"
        );
        let bare = decide(&Facts { cpuid_8000_0008_ebx: 0, ..AMD_17H_60H });
        assert_eq!(
            std::format!("{}", bare.line(SpecStoreBypass, Mitigations::NONE)),
            "vulnerable: needs SSBD for programs that ask; this CPU enumerates no SSBD for programs that ask"
        );
        let unlisted = decide(&Facts { signature: 0x306a9, ..NOTHING });
        assert_eq!(
            std::format!("{}", unlisted.line(ItlbMultihit, Mitigations::of(&[NoGuest]))),
            "mitigated: no guest runs (no vendor row names this CPU: assumed affected)"
        );
        let unknown = decide(&Facts { vendor: Vendor::Other(*b"HygonGenuine"), ..NOTHING });
        assert_eq!(
            std::format!("{}", unknown.line(SpectreV2, Mitigations::NONE)),
            "unknown: no guidance is read for this CPU's vendor"
        );
    }

    /// What every CPU has, what it applies on request where it enumerates
    /// it, and each control its register shows on: a register the CPU was
    /// not read for shows nothing.
    #[test]
    fn what_is_in_force_is_what_the_registers_show() {
        let unread = Registers { spec_ctrl: None, mcu_opt_ctrl: None, de_cfg2: None, c001_1029: None };
        let intel = decide(&INTEL_806C1);
        let always = ALWAYS.with(Ibpb).with(Ssbd);
        assert_eq!(intel.in_force(&unread), always);
        assert_eq!(intel.in_force(&Registers { spec_ctrl: Some(msr::IBRS), ..unread }), always.with(EnhancedIbrs));
        // A bit the decision does not name shows nothing.
        assert_eq!(intel.in_force(&Registers { spec_ctrl: Some(msr::BHI_DIS_S), ..unread }), always);
        let both = always.with(SrbdsMicrocode).with(GdsMicrocode);
        assert_eq!(intel.in_force(&Registers { mcu_opt_ctrl: Some(0), ..unread }), both);
        assert_eq!(intel.in_force(&Registers { mcu_opt_ctrl: Some(msr::GDS_MITG_DIS), ..unread }), always.with(SrbdsMicrocode));
        let opted_out = Registers { mcu_opt_ctrl: Some(msr::GDS_MITG_DIS | msr::RNGDS_MITG_DIS), ..unread };
        assert_eq!(intel.in_force(&opted_out), always);
        // What the CPU does not enumerate is not applied for anyone.
        let bare = decide(&Facts { cpuid_7_0_edx: 0, ..INTEL_806C1 });
        assert_eq!(bare.in_force(&unread), ALWAYS);
        let amd = decide(&AMD_17H_60H);
        let always = ALWAYS.with(Ibpb).with(ReturnPredictorFilled).with(Stibp).with(Ssbd);
        let both = Registers { de_cfg2: Some(msr::SUPPRESS_BP_ON_NON_BR), c001_1029: Some(C001_1029_LFENCE_SERIALIZES_DISPATCH), ..unread };
        assert_eq!(amd.in_force(&both), always.with(SuppressBpOnNonBr).with(LfenceDispatchSerializing));
        assert_eq!(amd.in_force(&Registers { de_cfg2: Some(0), c001_1029: Some(0), ..unread }), always);
    }

    /// What each CPU applies for a program that asked: SSBD alone on the
    /// T14's, whose enhanced IBRS keeps its branches from the core's other
    /// thread; SSBD, STIBP and the fill after IBPB on Zen 2's; and SSBD by
    /// `VIRT_SPEC_CTRL` in an AMD guest that enumerates no other.
    #[test]
    fn a_cpu_applies_on_request_what_its_decision_names() {
        let ssbd = OnRequest { spec_ctrl: msr::SSBD, virt_spec_ctrl: 0, ibpb: true, fill_after_ibpb: false };
        assert_eq!(decide(&INTEL_806C1).on_request(), ssbd);
        let zen2 = OnRequest { spec_ctrl: msr::SSBD | msr::STIBP, fill_after_ibpb: true, ..ssbd };
        assert_eq!(decide(&AMD_17H_60H).on_request(), zen2);
        assert_eq!(decide(&Facts { smt: false, ..AMD_17H_60H }).on_request(), OnRequest { spec_ctrl: msr::SSBD, ..zen2 });
        let guest = Facts { hypervisor: true, smt: false, cpuid_8000_0008_ebx: amd::IBPB | amd::SSBD_VIRT_SPEC_CTRL, ..AMD_17H_60H };
        let virt = OnRequest { spec_ctrl: 0, virt_spec_ctrl: msr::SSBD, ibpb: true, fill_after_ibpb: true };
        assert_eq!(decide(&guest).on_request(), virt);
        assert_eq!(decide(&NOTHING).on_request(), OnRequest::default());
    }

    /// A program asks as the program that started it does, or because its
    /// start asked.
    #[test]
    fn a_child_asks_as_its_spawner_or_its_start() {
        use Speculation::{Isolated, Shared};
        assert_eq!(Shared.of_child(false), Shared);
        assert_eq!(Shared.of_child(true), Isolated);
        assert_eq!(Isolated.of_child(false), Isolated);
        assert_eq!(Isolated.of_child(true), Isolated);
    }

    /// Spaces B, C and A, of which A asked, run in turn on one CPU: B, C,
    /// idle, B, A, idle, A, B. An IBPB goes before A, which arrives from B,
    /// and before B, which arrives from A: the switch back from idle to A
    /// stays in A's space, and no other switch involves it.
    #[test]
    fn an_ibpb_goes_between_two_spaces_one_of_which_asked() {
        use Speculation::{Isolated, Shared};
        let (a, b, c) = (Some((Pid(1), Isolated)), Some((Pid(2), Shared)), Some((Pid(3), Shared)));
        let mut last = LastAsked::new(None);
        let barriers = [b, c, None, b, a, None, a, b].map(|next| last.switch_to(next));
        assert_eq!(barriers, [false, false, false, false, true, false, false, true]);
        assert_eq!(last.space(), None);
        // Two spaces that both asked, and one asking space alone.
        let d = Some((Pid(4), Isolated));
        let mut last = LastAsked::new(None);
        let barriers = [a, d, a, None, a, a].map(|next| last.switch_to(next));
        assert_eq!(barriers, [true, true, true, false, false, false]);
        assert_eq!(last.space(), Some(Pid(1)));
    }

    /// The registers each verdict sets at bring-up: `IBRS` for enhanced IBRS
    /// and AMD's always-on IBRS, `BHI_DIS_S` where it is named, and the
    /// microcode opt-outs cleared where their mitigation is; and what a
    /// register that does not hold them leaves unapplied.
    #[test]
    fn each_verdict_sets_the_registers_its_mitigations_name() {
        let t14 = decide(&INTEL_806C1);
        assert_eq!(t14.spec_ctrl(), msr::IBRS);
        assert_eq!(t14.mcu_opt_ctrl_clears(), msr::RNGDS_MITG_DIS | msr::GDS_MITG_DIS);
        let set = Registers { spec_ctrl: Some(msr::IBRS), mcu_opt_ctrl: Some(0), de_cfg2: None, c001_1029: None };
        assert_eq!(t14.unapplied(&set), Mitigations::NONE);
        let firmware = Registers { spec_ctrl: Some(0), mcu_opt_ctrl: Some(msr::GDS_MITG_DIS), ..set };
        assert_eq!(t14.unapplied(&firmware), Mitigations::of(&[EnhancedIbrs, GdsMicrocode]));
        // Enhanced IBRS sets a bit only CPUID.(07H,0):EDX[26] enumerates.
        let no_ibrs = decide(&Facts { cpuid_7_0_edx: INTEL_806C1.cpuid_7_0_edx & !IBRS_IBPB, ..INTEL_806C1 });
        assert_eq!(no_ibrs.spec_ctrl(), 0);
        assert_eq!(no_ibrs.verdict(SpectreV2), lacking(&[EnhancedIbrs, Smep, Ibpb], &[EnhancedIbrs, Ibpb]));

        let bhi_ctrl = decide(&Facts { cpuid_7_2_edx: BHI_CTRL, ..INTEL_806C1 });
        assert_eq!(bhi_ctrl.spec_ctrl(), msr::IBRS | msr::BHI_DIS_S);

        let zen2 = decide(&AMD_17H_60H);
        assert_eq!((zen2.spec_ctrl(), zen2.mcu_opt_ctrl_clears()), (0, 0));
        let preferred = amd::IBRS | amd::IBRS_ALWAYS_ON | amd::IBRS_PREFERRED;
        let always_on = decide(&Facts { cpuid_8000_0008_ebx: AMD_17H_60H.cpuid_8000_0008_ebx | preferred, ..AMD_17H_60H });
        assert_eq!(always_on.spec_ctrl(), msr::IBRS);
        let lfence = Registers { spec_ctrl: Some(0), mcu_opt_ctrl: None, de_cfg2: None, c001_1029: Some(0) };
        assert_eq!(always_on.unapplied(&lfence), Mitigations::of(&[LfenceDispatchSerializing, IbrsAlwaysOn]));
    }

    /// Which CPUs have `IA32_SPEC_CTRL` at all.
    #[test]
    fn spec_ctrl_exists_where_a_bit_of_it_is_enumerated() {
        assert!(INTEL_806C1.has_spec_ctrl());
        assert!(AMD_17H_60H.has_spec_ctrl());
        assert!(!NOTHING.has_spec_ctrl());
        assert!(Facts { cpuid_7_2_edx: PSFD, ..NOTHING }.has_spec_ctrl());
        assert!(!Facts { cpuid_7_0_edx: MD_CLEAR | L1D_FLUSH, ..NOTHING }.has_spec_ctrl());
        assert!(!Facts { vendor: Vendor::Amd, cpuid_8000_0008_ebx: amd::IBPB, ..NOTHING }.has_spec_ctrl());
        assert!(!Facts { vendor: Vendor::Other(*b"HygonGenuine"), ..AMD_17H_60H }.has_spec_ctrl());
    }

    /// The set's bits are the enum's discriminants, so a mitigation left
    /// out of `ALL` would be needed and never named.
    #[test]
    fn every_mitigation_is_named_once() {
        for (at, one) in Mitigation::ALL.into_iter().enumerate() {
            assert_eq!(one as usize, at, "{one:?}");
        }
        for (at, of) in Vulnerability::ALL.into_iter().enumerate() {
            assert_eq!(of as usize, at, "{of:?}");
        }
    }

    #[test]
    fn the_tables_rows_are_one_per_cpu() {
        for pair in INTEL_ROWS.windows(2) {
            assert!(pair[0].signature < pair[1].signature, "{:#x} then {:#x}", pair[0].signature, pair[1].signature);
        }
        for row in &INTEL_ROWS {
            assert_eq!(row.signature & !row.mask, 0, "{:#x}", row.signature);
            let covered = INTEL_ROWS.iter().filter(|other| row.signature & other.mask == other.signature).count();
            assert_eq!(covered, 1, "{:#x}", row.signature);
        }
    }
}
