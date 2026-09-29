//! Linux's decision about a CPU's speculative-execution vulnerabilities, pure.
//!
//! [`decide`] maps what a CPU reports about itself ([`Facts`]) to the line the
//! pinned Linux prints in each `/sys/devices/system/cpu/vulnerabilities/*` file
//! ([`Decision::line`]) and to the mitigation state it selects ([`Decision`]'s
//! fields), under the default command line and the pinned config. That Linux is
//! tag `Ubuntu-6.8.0-142.142` (commit 53e5d07aac02) of Ubuntu's noble kernel
//! with `/boot/config-6.8.0-142-generic`; every `path:line` here is read there,
//! and `common.c`, `bugs.c`, `intel.c`, `amd.c` and `tsx.c` are in
//! `arch/x86/kernel/cpu/`. The config's `CONFIG_CPU_UNRET_ENTRY`,
//! `CONFIG_CPU_IBPB_ENTRY`, `CONFIG_CPU_SRSO`, `CONFIG_MITIGATION_TSA` and
//! `CONFIG_MITIGATION_VMSCAPE` are `y`
//! (`debian.master/config/annotations:3348,3319,3339,8048,411`).
//!
//! Parity is exact or refused: an input whose answer rests on something the
//! facts do not carry is [`Refused`] whole, and a line that does is
//! [`Unmodelled`], never approximated.
//!
//! Pure: no I/O, no allocation, no `unsafe`. The caller reads the facts.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

mod table;
#[cfg(test)]
mod tests;

use table::{
    blacklisted, whitelisted, GDS, ITS, MMIO, MMIO_SBDS, MSBDS_ONLY, NO_BHI,
    NO_EIBRS_PBRSB, NO_ITLB_MULTIHIT, NO_L1TF, NO_MDS, NO_MELTDOWN, NO_MMIO, NO_SPECTRE_V2,
    NO_SPECULATION, NO_SSB, RETBLEED, RFDS, SRBDS, SRSO, TSA, VMSCAPE,
};

/// The vendors Linux's tables name, by CPUID.0's identification string
/// (`get_cpu_vendor`, `common.c:912-936`, over each `cpu_dev`'s `c_ident`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    Hygon,
    Centaur,
    Zhaoxin,
    Unknown,
}

impl Vendor {
    /// CPUID.0's EBX, EDX and ECX bytes, in that order.
    pub const fn from_id(id: &[u8; 12]) -> Self {
        match id {
            b"GenuineIntel" => Self::Intel,
            b"AuthenticAMD" => Self::Amd,
            b"HygonGenuine" => Self::Hygon,
            b"CentaurHauls" => Self::Centaur,
            b"  Shanghai  " => Self::Zhaoxin,
            _ => Self::Unknown,
        }
    }
}

/// What the boot CPU reports, read the way Linux reads it.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    pub vendor: Vendor,
    /// CPUID.1:EAX; family, model and stepping are derived from it as
    /// `arch/x86/lib/cpu.c` derives them.
    pub signature: u32,
    /// The microcode revision as `/proc/cpuinfo` reports it.
    pub microcode: u32,
    pub cpuid_1_ecx: u32,
    pub cpuid_7_0_ebx: u32,
    pub cpuid_7_0_edx: u32,
    /// Read wherever leaf 7 exists (`scattered.c:59-79`).
    pub cpuid_7_2_edx: u32,
    /// 0 unless CPUID.0x80000000:EAX reaches 0x80000008 (`common.c:1055-1084`).
    pub cpuid_8000_0008_ebx: u32,
    /// 0 unless CPUID.0x80000000:EAX reaches 0x80000021 (`common.c:1055-1084`).
    pub cpuid_8000_0021_eax: u32,
    /// 0 unless CPUID.0x80000000:EAX reaches 0x80000021 (`scattered.c:52-53`).
    pub cpuid_8000_0021_ecx: u32,
    /// `IA32_ARCH_CAPABILITIES` (0x10A), 0 unless CPUID.(7,0):EDX[29]
    /// enumerates it (`common.c:1353-1361`): the read is `#GP` there.
    pub arch_capabilities: u64,
    /// `IA32_MCU_OPT_CTRL` (0x123), present exactly where
    /// `ARCH_CAPABILITIES.GDS_CTRL` enumerates it.
    pub mcu_opt_ctrl: Option<u64>,
    /// Whether `rdmsr` of `MSR_AMD64_LS_CFG` (0xC0011020) completes without
    /// `#GP`, present exactly where `bsp_init_amd` probes it (`amd.c:576-596`):
    /// AMD family 0x15 to 0x17 enumerating neither `AMD_SSBD` nor `VIRT_SSBD`.
    pub ls_cfg_readable: Option<bool>,
    /// Whether `wrmsr` of `IA32_PRED_CMD` with `SBPB` (bit 7) completes without
    /// `#GP`, present exactly where `early_init_amd` probes it
    /// (`amd.c:799-806`): AMD from family 0x19, outside a hypervisor, without
    /// CPUID's `IBPB_BRTYPE`.
    pub sbpb_write_accepted: Option<bool>,
    /// More than one thread per core, every sibling online: `sched_smt_active()`,
    /// which under the default command line is also `cpu_smt_possible()`.
    pub smt: bool,
}

// Linux's feature bits (`arch/x86/include/asm/cpufeatures.h`,
// `scattered.c:26-57`) at their CPUID positions.
const CPUID_1_ECX_AVX: u32 = 1 << 28;
const CPUID_1_ECX_RDRAND: u32 = 1 << 30;
const CPUID_1_ECX_HYPERVISOR: u32 = 1 << 31;
const CPUID_7_0_EBX_RTM: u32 = 1 << 11;
const CPUID_7_0_EBX_RDSEED: u32 = 1 << 18;
const CPUID_7_0_EDX_SRBDS_CTRL: u32 = 1 << 9;
const CPUID_7_0_EDX_MD_CLEAR: u32 = 1 << 10;
const CPUID_7_0_EDX_RTM_ALWAYS_ABORT: u32 = 1 << 11;
const CPUID_7_0_EDX_SPEC_CTRL: u32 = 1 << 26;
const CPUID_7_0_EDX_INTEL_STIBP: u32 = 1 << 27;
const CPUID_7_0_EDX_FLUSH_L1D: u32 = 1 << 28;
const CPUID_7_0_EDX_ARCH_CAPABILITIES: u32 = 1 << 29;
const CPUID_7_0_EDX_SPEC_CTRL_SSBD: u32 = 1 << 31;
const CPUID_7_2_EDX_RRSBA_CTRL: u32 = 1 << 2;
const CPUID_7_2_EDX_BHI_CTRL: u32 = 1 << 4;
const CPUID_8000_0008_EBX_AMD_IBPB: u32 = 1 << 12;
const CPUID_8000_0008_EBX_AMD_IBRS: u32 = 1 << 14;
const CPUID_8000_0008_EBX_AMD_STIBP: u32 = 1 << 15;
const CPUID_8000_0008_EBX_AMD_STIBP_ALWAYS_ON: u32 = 1 << 17;
const CPUID_8000_0008_EBX_AMD_SSBD: u32 = 1 << 24;
const CPUID_8000_0008_EBX_VIRT_SSBD: u32 = 1 << 25;
const CPUID_8000_0008_EBX_AMD_SSB_NO: u32 = 1 << 26;
const CPUID_8000_0008_EBX_BTC_NO: u32 = 1 << 29;
const CPUID_8000_0021_EAX_VERW_CLEAR: u32 = 1 << 5;
const CPUID_8000_0021_EAX_AUTOIBRS: u32 = 1 << 8;
const CPUID_8000_0021_EAX_SBPB: u32 = 1 << 27;
const CPUID_8000_0021_EAX_IBPB_BRTYPE: u32 = 1 << 28;
const CPUID_8000_0021_EAX_SRSO_NO: u32 = 1 << 29;
const CPUID_8000_0021_EAX_SRSO_USER_KERNEL_NO: u32 = 1 << 30;
const CPUID_8000_0021_EAX_SRSO_BP_SPEC_REDUCE: u32 = 1 << 31;
const CPUID_8000_0021_ECX_TSA_SQ_NO: u32 = 1 << 1;
const CPUID_8000_0021_ECX_TSA_L1_NO: u32 = 1 << 2;

// `arch/x86/include/asm/msr-index.h:46-54,101-183,215`.
const ARCH_CAP_RDCL_NO: u64 = 1 << 0;
const ARCH_CAP_IBRS_ALL: u64 = 1 << 1;
const ARCH_CAP_RSBA: u64 = 1 << 2;
const ARCH_CAP_SSB_NO: u64 = 1 << 4;
const ARCH_CAP_MDS_NO: u64 = 1 << 5;
const ARCH_CAP_PSCHANGE_MC_NO: u64 = 1 << 6;
const ARCH_CAP_TSX_CTRL_MSR: u64 = 1 << 7;
const ARCH_CAP_TAA_NO: u64 = 1 << 8;
const ARCH_CAP_SBDR_SSDP_NO: u64 = 1 << 13;
const ARCH_CAP_FBSDP_NO: u64 = 1 << 14;
const ARCH_CAP_PSDP_NO: u64 = 1 << 15;
const ARCH_CAP_FB_CLEAR: u64 = 1 << 17;
const ARCH_CAP_RRSBA: u64 = 1 << 19;
const ARCH_CAP_PBRSB_NO: u64 = 1 << 24;
const ARCH_CAP_GDS_CTRL: u64 = 1 << 25;
const ARCH_CAP_GDS_NO: u64 = 1 << 26;
const ARCH_CAP_RFDS_NO: u64 = 1 << 27;
const ARCH_CAP_RFDS_CLEAR: u64 = 1 << 28;
const ARCH_CAP_ITS_NO: u64 = 1 << 62;
const GDS_MITG_LOCKED: u64 = 1 << 5;
pub const SPEC_CTRL_IBRS: u64 = 1 << 0;
pub const SPEC_CTRL_STIBP: u64 = 1 << 1;
pub const SPEC_CTRL_RRSBA_DIS_S: u64 = 1 << 6;
pub const SPEC_CTRL_BHI_DIS_S: u64 = 1 << 10;

/// `boot_cpu_data`'s identity: `x86_vendor`, `x86`, `x86_model`,
/// `x86_stepping`.
pub(crate) struct Ident {
    pub(crate) vendor: Vendor,
    pub(crate) family: u32,
    pub(crate) model: u32,
    pub(crate) stepping: u32,
}

impl Ident {
    /// `x86_family`, `x86_model` and `x86_stepping` (`arch/x86/lib/cpu.c:6-37`).
    fn new(vendor: Vendor, sig: u32) -> Self {
        let mut family = (sig >> 8) & 0xf;
        if family == 0xf {
            family += (sig >> 20) & 0xff;
        }
        let mut model = (sig >> 4) & 0xf;
        if family >= 0x6 {
            model += ((sig >> 16) & 0xf) << 4;
        }
        Self { vendor, family, model, stepping: sig & 0xf }
    }
}

/// An input this crate does not decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// `bsp_init_hygon`'s `MSR_AMD64_LS_CFG` probe (`hygon.c:228-239`), which
    /// [`Facts::ls_cfg_readable`] does not carry.
    Hygon,
}

/// An affected CPU's line that rests on state the facts do not carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmodelled {
    /// The e820 map against `x86_cache_bits` (`bugs.c:2538-2583`) and
    /// `kvm_intel`'s state (`bugs.c:3074-3089`).
    L1tf,
    /// `IA32_FEAT_CTL` and `CR4.VMXE` (`bugs.c:3091-3102`).
    ItlbMultihit,
}

/// The vulnerabilities files, in the order `drivers/base/cpu.c:615-631` lists
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vuln {
    Meltdown,
    SpectreV1,
    SpectreV2,
    SpecStoreBypass,
    L1tf,
    Mds,
    TsxAsyncAbort,
    ItlbMultihit,
    Srbds,
    MmioStaleData,
    Retbleed,
    SpecRstackOverflow,
    GatherDataSampling,
    RegFileDataSampling,
    IndirectTargetSelection,
    Tsa,
    Vmscape,
}

impl Vuln {
    pub const ALL: [Self; 17] = [
        Self::Meltdown,
        Self::SpectreV1,
        Self::SpectreV2,
        Self::SpecStoreBypass,
        Self::L1tf,
        Self::Mds,
        Self::TsxAsyncAbort,
        Self::ItlbMultihit,
        Self::Srbds,
        Self::MmioStaleData,
        Self::Retbleed,
        Self::SpecRstackOverflow,
        Self::GatherDataSampling,
        Self::RegFileDataSampling,
        Self::IndirectTargetSelection,
        Self::Tsa,
        Self::Vmscape,
    ];

    /// The file's name under `/sys/devices/system/cpu/vulnerabilities/`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Meltdown => "meltdown",
            Self::SpectreV1 => "spectre_v1",
            Self::SpectreV2 => "spectre_v2",
            Self::SpecStoreBypass => "spec_store_bypass",
            Self::L1tf => "l1tf",
            Self::Mds => "mds",
            Self::TsxAsyncAbort => "tsx_async_abort",
            Self::ItlbMultihit => "itlb_multihit",
            Self::Srbds => "srbds",
            Self::MmioStaleData => "mmio_stale_data",
            Self::Retbleed => "retbleed",
            Self::SpecRstackOverflow => "spec_rstack_overflow",
            Self::GatherDataSampling => "gather_data_sampling",
            Self::RegFileDataSampling => "reg_file_data_sampling",
            Self::IndirectTargetSelection => "indirect_target_selection",
            Self::Tsa => "tsa",
            Self::Vmscape => "vmscape",
        }
    }
}

/// `tsx_ctrl_state` after `tsx_init` (`tsx.c:158-245`) under
/// `CONFIG_X86_INTEL_TSX_MODE_OFF=y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tsx {
    /// No `IA32_TSX_CTRL`: RTM stays as CPUID enumerates it.
    NotSupported,
    /// RTM always aborts; its enumeration is cleared.
    RtmAlwaysAbort,
    /// `IA32_TSX_CTRL` disables RTM and clears its enumeration.
    Disable,
}

/// `spectre_v2_enabled` (`bugs.c:1860-2045`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpectreV2 {
    None,
    Retpoline,
    Eibrs,
    Ibrs,
}

/// What `bhi_select_mitigation` (`bugs.c:1831-1858`) chose, as
/// `spectre_bhi_state` (`bugs.c:3220-3236`) reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bhi {
    NotAffected,
    /// `SPEC_CTRL.BHI_DIS_S`.
    BhiDisS,
    /// `clear_bhb_loop` on syscall entry and VM exit.
    SwLoop,
    /// Retpolines with RRSBA disabled.
    Retpoline,
    Vulnerable,
}

/// Whether and how `switch_mm` issues IBPB (`bugs.c:1527-1550`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ibpb {
    /// The CPU has no IBPB.
    Absent,
    Disabled,
    /// Between two processes when either asked for it by prctl.
    Conditional,
}

/// `spectre_v2_user_stibp` (`bugs.c:1492-1591`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stibp {
    None,
    Prctl,
    StrictPreferred,
}

/// `retbleed_mitigation` (`bugs.c:1054-1194`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retbleed {
    None,
    Unret,
    Ibrs,
    Eibrs,
}

/// `ssb_mode` (`bugs.c:2172-2238`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ssb {
    None,
    Prctl,
}

/// `mds_mitigation` (`bugs.c:266-283`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mds {
    Off,
    Full,
    Vmwerv,
}

/// `taa_mitigation` (`bugs.c:327-382`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taa {
    Off,
    UcodeNeeded,
    Verw,
    TsxDisabled,
}

/// `mmio_mitigation` (`bugs.c:424-478`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mmio {
    Off,
    UcodeNeeded,
    Verw,
}

/// `rfds_mitigation` (`bugs.c:520-533`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rfds {
    Off,
    Verw,
    UcodeNeeded,
}

/// `srbds_mitigation` where the CPU has SRBDS (`bugs.c:678-700`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Srbds {
    UcodeNeeded,
    Full,
    TsxOff,
    Hypervisor,
}

/// `gds_mitigation` where the CPU has GDS (`bugs.c:814-867`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gds {
    UcodeNeeded,
    /// `IA32_MCU_OPT_CTRL.GDS_MITG_DIS` cleared on every CPU.
    Full,
    /// Firmware locked the mitigation on.
    FullLocked,
    Hypervisor,
}

/// `its_mitigation` (`bugs.c:1254-1335`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Its {
    Off,
    AlignedThunks,
}

/// `vmscape_mitigation` (`bugs.c:2849-2869`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vmscape {
    None,
    IbpbExitToUser,
    IbpbOnVmexit,
}

/// `srso_mitigation` where the CPU has SRSO (`bugs.c:2670-2807`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Srso {
    /// `SRSO_NO` forced: Zen1/2 with the IBPB microcode and no SMT.
    SmtDisabled,
    UcodeNeeded,
    /// The safe-RET return thunk without the IBPB microcode.
    SafeRetUcodeNeeded,
    /// The safe-RET return thunk: `srso_alias_return_thunk` on family 0x19,
    /// `srso_return_thunk` otherwise.
    SafeRet,
    IbpbOnVmexit,
    BpSpecReduce,
}

/// `tsa_mitigation` where the CPU has TSA (`bugs.c:2918-2962`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tsa {
    UcodeNeeded,
    Full,
}

/// The bugs `cpu_set_bug_bits` (`common.c:1414-1578`) sets that a line or a
/// selection reads.
#[derive(Clone, Copy, Debug, Default)]
struct Bugs {
    itlb_multihit: bool,
    spectre_v1: bool,
    spectre_v2: bool,
    ssb: bool,
    ibrs_enhanced: bool,
    eibrs_pbrsb: bool,
    mds: bool,
    msbds_only: bool,
    taa: bool,
    srbds: bool,
    mmio_stale_data: bool,
    mmio_unknown: bool,
    retbleed: bool,
    gds: bool,
    rfds: bool,
    bhi: bool,
    vmscape: bool,
    its: bool,
    srso: bool,
    tsa: bool,
    meltdown: bool,
    l1tf: bool,
}

/// The features Linux has when it selects, after `init_speculation_control`
/// (`common.c:973-1012`), `early_init_intel`'s microcode check
/// (`intel.c:296-309`) and `bsp_init_amd`'s `LS_CFG_SSBD` (`amd.c:576-596`).
struct Caps {
    hypervisor: bool,
    ibrs: bool,
    ibpb: bool,
    stibp: bool,
    ssbd: bool,
    autoibrs: bool,
}

/// Linux's selection for one machine.
#[derive(Clone, Copy, Debug)]
pub struct Decision {
    pub tsx: Tsx,
    pub spectre_v2: SpectreV2,
    pub bhi: Bhi,
    pub ibpb: Ibpb,
    /// `USE_IBRS_FW`: IBRS around firmware calls.
    pub ibrs_fw: bool,
    pub stibp: Stibp,
    pub retbleed: Retbleed,
    pub ssb: Ssb,
    pub mds: Mds,
    pub taa: Taa,
    pub mmio: Mmio,
    pub rfds: Rfds,
    pub srbds: Option<Srbds>,
    pub gds: Option<Gds>,
    pub its: Its,
    pub vmscape: Vmscape,
    pub srso: Option<Srso>,
    pub tsa: Option<Tsa>,
    /// `x86_pred_cmd` is `PRED_CMD_SBPB` rather than `PRED_CMD_IBPB`
    /// (`bugs.c:2677-2678`).
    pub sbpb: bool,
    /// `CLEAR_CPU_BUF`: `verw` on every return to user.
    pub clear_cpu_buf: bool,
    /// `EFER.AUTOIBRS` in place of `SPEC_CTRL.IBRS`.
    pub efer_autoibrs: bool,
    /// The bits `bugs.c` ORs into `x86_spec_ctrl_base`; the MSR's others are
    /// the hardware's.
    pub spec_ctrl: u64,
    bugs: Bugs,
    hypervisor: bool,
    autoibrs: bool,
    smt: bool,
}

/// `cpu_set_bug_bits` (`common.c:1414-1578`). `SWAPGS`, `SMT_RSB` and
/// `IBPB_NO_RET` reach no line and no selection here.
fn bug_bits(f: &Facts, id: &Ident, caps: &Caps, arch: u64) -> Bugs {
    let mut b = Bugs {
        itlb_multihit: !whitelisted(id, NO_ITLB_MULTIHIT) && arch & ARCH_CAP_PSCHANGE_MC_NO == 0,
        ..Bugs::default()
    };
    if whitelisted(id, NO_SPECULATION) {
        return b;
    }
    b.spectre_v1 = true;
    b.spectre_v2 = !whitelisted(id, NO_SPECTRE_V2);
    b.ssb = !whitelisted(id, NO_SSB)
        && arch & ARCH_CAP_SSB_NO == 0
        && f.cpuid_8000_0008_ebx & CPUID_8000_0008_EBX_AMD_SSB_NO == 0;
    b.ibrs_enhanced = arch & ARCH_CAP_IBRS_ALL != 0 || caps.autoibrs;
    b.eibrs_pbrsb =
        b.ibrs_enhanced && !whitelisted(id, NO_EIBRS_PBRSB) && arch & ARCH_CAP_PBRSB_NO == 0;
    b.mds = !whitelisted(id, NO_MDS) && arch & ARCH_CAP_MDS_NO == 0;
    b.msbds_only = b.mds && whitelisted(id, MSBDS_ONLY);
    b.taa = arch & ARCH_CAP_TAA_NO == 0
        && (f.cpuid_7_0_ebx & CPUID_7_0_EBX_RTM != 0 || arch & ARCH_CAP_TSX_CTRL_MSR != 0);
    b.srbds = (f.cpuid_1_ecx & CPUID_1_ECX_RDRAND != 0 || f.cpuid_7_0_ebx & CPUID_7_0_EBX_RDSEED != 0)
        && blacklisted(id, SRBDS | MMIO_SBDS);
    let mmio_immune = arch & (ARCH_CAP_FBSDP_NO | ARCH_CAP_PSDP_NO | ARCH_CAP_SBDR_SSDP_NO)
        == ARCH_CAP_FBSDP_NO | ARCH_CAP_PSDP_NO | ARCH_CAP_SBDR_SSDP_NO;
    if !mmio_immune {
        if blacklisted(id, MMIO) {
            b.mmio_stale_data = true;
        } else if !whitelisted(id, NO_MMIO) {
            b.mmio_unknown = true;
        }
    }
    b.retbleed = f.cpuid_8000_0008_ebx & CPUID_8000_0008_EBX_BTC_NO == 0
        && (blacklisted(id, RETBLEED) || arch & ARCH_CAP_RSBA != 0);
    b.gds = blacklisted(id, GDS)
        && arch & ARCH_CAP_GDS_NO == 0
        && f.cpuid_1_ecx & CPUID_1_ECX_AVX != 0;
    // `vulnerable_to_rfds` (`common.c:1370-1386`).
    b.rfds = if arch & ARCH_CAP_RFDS_NO != 0 {
        false
    } else {
        arch & ARCH_CAP_RFDS_CLEAR != 0 || blacklisted(id, RFDS)
    };
    b.bhi = !whitelisted(id, NO_BHI) && (b.ibrs_enhanced || caps.hypervisor);
    b.vmscape = blacklisted(id, VMSCAPE) && !caps.hypervisor;
    // `vulnerable_to_its` (`common.c:1388-1412`). `ITS_NATIVE_ONLY` reaches no
    // line under the default command line.
    b.its = arch & ARCH_CAP_ITS_NO == 0
        && id.vendor == Vendor::Intel
        && f.cpuid_7_2_edx & CPUID_7_2_EDX_BHI_CTRL == 0
        && (caps.hypervisor || blacklisted(id, ITS));
    b.srso = f.cpuid_8000_0021_eax & CPUID_8000_0021_EAX_SRSO_NO == 0 && blacklisted(id, SRSO);
    // `tsa_init`'s forced `TSA_*_NO` (`amd.c:517-530`) reaches no family the
    // TSA row names, and the Zen-guest clause reads `X86_FEATURE_ZEN`, which
    // `init_amd` sets (`amd.c:1039`) from `identify_cpu` (`common.c:1997-1998`),
    // after `early_identify_cpu` has run this (`common.c:1732`).
    b.tsa = f.cpuid_8000_0021_ecx & (CPUID_8000_0021_ECX_TSA_SQ_NO | CPUID_8000_0021_ECX_TSA_L1_NO)
        != CPUID_8000_0021_ECX_TSA_SQ_NO | CPUID_8000_0021_ECX_TSA_L1_NO
        && blacklisted(id, TSA);
    if whitelisted(id, NO_MELTDOWN) || arch & ARCH_CAP_RDCL_NO != 0 {
        return b;
    }
    b.meltdown = true;
    b.l1tf = !whitelisted(id, NO_L1TF);
    b
}

/// `spec_ctrl_disable_kernel_rrsba` (`bugs.c:1723-1739`).
fn disable_kernel_rrsba(f: &Facts, arch: u64, spec_ctrl: &mut u64, rrsba_disabled: &mut bool) {
    if *rrsba_disabled {
        return;
    }
    if arch & ARCH_CAP_RRSBA == 0 {
        *rrsba_disabled = true;
        return;
    }
    if f.cpuid_7_2_edx & CPUID_7_2_EDX_RRSBA_CTRL == 0 {
        return;
    }
    *spec_ctrl |= SPEC_CTRL_RRSBA_DIS_S;
    *rrsba_disabled = true;
}

/// Linux's selection over `facts`: `cpu_select_mitigations` (`bugs.c:149-199`)
/// and what it reads.
///
/// Panics where the facts contradict their own documentation: an
/// `arch_capabilities` read without CPUID.(7,0):EDX[29], or an `mcu_opt_ctrl`,
/// `ls_cfg_readable` or `sbpb_write_accepted` present where Linux does not read
/// it, or absent where it does.
pub fn decide(facts: &Facts) -> Result<Decision, Refused> {
    let f = facts;
    let id = Ident::new(f.vendor, f.signature);
    if id.vendor == Vendor::Hygon {
        return Err(Refused::Hygon);
    }
    let amd = id.vendor == Vendor::Amd;
    assert!(
        f.cpuid_7_0_edx & CPUID_7_0_EDX_ARCH_CAPABILITIES != 0 || f.arch_capabilities == 0,
        "ARCH_CAPABILITIES {:#x} without CPUID.(7,0):EDX[29]",
        f.arch_capabilities
    );
    let arch = f.arch_capabilities;
    assert_eq!(
        f.mcu_opt_ctrl.is_some(),
        arch & ARCH_CAP_GDS_CTRL != 0,
        "IA32_MCU_OPT_CTRL is read exactly where ARCH_CAPABILITIES.GDS_CTRL"
    );

    let edx7 = f.cpuid_7_0_edx;
    let ebx8 = f.cpuid_8000_0008_ebx;
    let eax21 = f.cpuid_8000_0021_eax;
    let hypervisor = f.cpuid_1_ecx & CPUID_1_ECX_HYPERVISOR != 0;
    let amd_ssbd = ebx8 & (CPUID_8000_0008_EBX_VIRT_SSBD | CPUID_8000_0008_EBX_AMD_SSBD) != 0;
    assert_eq!(
        f.ls_cfg_readable.is_some(),
        amd && (0x15..=0x17).contains(&id.family) && !amd_ssbd,
        "MSR_AMD64_LS_CFG is probed exactly where amd.c:576-578 probes it"
    );
    let brtype_enumerated = eax21 & CPUID_8000_0021_EAX_IBPB_BRTYPE != 0;
    assert_eq!(
        f.sbpb_write_accepted.is_some(),
        amd && id.family >= 0x19 && !hypervisor && !brtype_enumerated,
        "PRED_CMD.SBPB is probed exactly where amd.c:799-802 probes it"
    );
    let spec_ctrl_bit = edx7 & CPUID_7_0_EDX_SPEC_CTRL != 0;
    let intel_stibp = edx7 & CPUID_7_0_EDX_INTEL_STIBP != 0;
    let mut caps = Caps {
        hypervisor,
        ibrs: spec_ctrl_bit || ebx8 & CPUID_8000_0008_EBX_AMD_IBRS != 0,
        ibpb: spec_ctrl_bit || ebx8 & CPUID_8000_0008_EBX_AMD_IBPB != 0,
        stibp: intel_stibp || ebx8 & CPUID_8000_0008_EBX_AMD_STIBP != 0,
        ssbd: edx7 & CPUID_7_0_EDX_SPEC_CTRL_SSBD != 0
            || amd_ssbd
            || f.ls_cfg_readable == Some(true),
        autoibrs: eax21 & CPUID_8000_0021_EAX_AUTOIBRS != 0,
    };
    // `early_init_amd` (`amd.c:799-806`).
    let native_brtype = amd
        && !hypervisor
        && ((id.family == 0x17 && ebx8 & CPUID_8000_0008_EBX_AMD_IBPB != 0)
            || f.sbpb_write_accepted == Some(true));
    let ibpb_brtype = brtype_enumerated || native_brtype;
    let sbpb = eax21 & CPUID_8000_0021_EAX_SBPB != 0 || f.sbpb_write_accepted == Some(true);
    if id.vendor == Vendor::Intel
        && !hypervisor
        && (spec_ctrl_bit || intel_stibp || caps.ibrs || caps.ibpb || caps.stibp)
        && table::bad_spectre_microcode(&id, f.microcode)
    {
        caps.ibrs = false;
        caps.ibpb = false;
        caps.stibp = false;
        caps.ssbd = false;
    }
    let bugs = bug_bits(f, &id, &caps, arch);

    let tsx = if edx7 & CPUID_7_0_EDX_RTM_ALWAYS_ABORT != 0 {
        Tsx::RtmAlwaysAbort
    } else if arch & ARCH_CAP_TSX_CTRL_MSR != 0 {
        Tsx::Disable
    } else {
        Tsx::NotSupported
    };
    let rtm = f.cpuid_7_0_ebx & CPUID_7_0_EBX_RTM != 0 && tsx == Tsx::NotSupported;

    let mut spec_ctrl = 0;
    let mut rrsba_disabled = false;
    let mut efer_autoibrs = false;

    // `spectre_v2_select_mitigation` (`bugs.c:1860-2045`). Where the CPU is
    // unaffected it returns before any of it, leaving `spectre_v2_cmd` at
    // `SPECTRE_V2_CMD_NONE`.
    let mut spectre_v2 = SpectreV2::None;
    let mut bhi = if bugs.bhi { Bhi::Vulnerable } else { Bhi::NotAffected };
    let mut ibrs_fw = false;
    if bugs.spectre_v2 {
        spectre_v2 = if bugs.ibrs_enhanced {
            SpectreV2::Eibrs
        } else if bugs.retbleed && caps.ibrs && id.vendor == Vendor::Intel {
            SpectreV2::Ibrs
        } else {
            SpectreV2::Retpoline
        };
        let ibrs_mode = matches!(spectre_v2, SpectreV2::Eibrs | SpectreV2::Ibrs);
        if ibrs_mode {
            if caps.autoibrs {
                efer_autoibrs = true;
            } else {
                spec_ctrl |= SPEC_CTRL_IBRS;
            }
        }
        if spectre_v2 == SpectreV2::Retpoline {
            disable_kernel_rrsba(f, arch, &mut spec_ctrl, &mut rrsba_disabled);
        }
        // `bhi_select_mitigation` (`bugs.c:1831-1858`); its own call of
        // `spec_ctrl_disable_kernel_rrsba` repeats the one just above.
        if bugs.bhi {
            bhi = if spectre_v2 == SpectreV2::Retpoline && rrsba_disabled {
                Bhi::Retpoline
            } else if f.cpuid_7_2_edx & CPUID_7_2_EDX_BHI_CTRL != 0 {
                spec_ctrl |= SPEC_CTRL_BHI_DIS_S;
                Bhi::BhiDisS
            } else {
                Bhi::SwLoop
            };
        }
        ibrs_fw =
            !(bugs.retbleed && caps.ibpb && id.vendor == Vendor::Amd) && caps.ibrs && !ibrs_mode;
    }

    // `retbleed_select_mitigation` (`bugs.c:1054-1194`).
    let retbleed = if !bugs.retbleed {
        Retbleed::None
    } else if id.vendor == Vendor::Intel {
        match spectre_v2 {
            SpectreV2::Ibrs => Retbleed::Ibrs,
            SpectreV2::Eibrs => Retbleed::Eibrs,
            SpectreV2::None | SpectreV2::Retpoline => Retbleed::None,
        }
    } else if id.vendor == Vendor::Amd {
        Retbleed::Unret
    } else {
        Retbleed::None
    };

    // `spectre_v2_user_select_mitigation` (`bugs.c:1492-1591`) and
    // `cpu_bugs_smt_update` (`bugs.c:2964-3058`).
    let user_cmd_none = !bugs.spectre_v2;
    let ibpb = if !caps.ibpb {
        Ibpb::Absent
    } else if user_cmd_none {
        Ibpb::Disabled
    } else {
        Ibpb::Conditional
    };
    let stibp = if user_cmd_none
        || !caps.stibp
        || !f.smt
        || (matches!(spectre_v2, SpectreV2::Eibrs) && !caps.autoibrs)
    {
        Stibp::None
    } else if ebx8 & CPUID_8000_0008_EBX_AMD_STIBP_ALWAYS_ON != 0 || retbleed == Retbleed::Unret {
        spec_ctrl |= SPEC_CTRL_STIBP;
        Stibp::StrictPreferred
    } else {
        Stibp::Prctl
    };

    let ssb = if caps.ssbd && bugs.ssb { Ssb::Prctl } else { Ssb::None };

    // `md_clear_select_mitigation` (`bugs.c:603-616`). Its update pass
    // (`bugs.c:555-601`) changes nothing under the default command line: each
    // bug it selects for again is already selected.
    let md_clear = edx7 & CPUID_7_0_EDX_MD_CLEAR != 0;
    let mut clear_cpu_buf = false;
    let mds = if !bugs.mds {
        Mds::Off
    } else {
        clear_cpu_buf = true;
        if md_clear { Mds::Full } else { Mds::Vmwerv }
    };
    let taa = if !bugs.taa {
        Taa::Off
    } else if !rtm {
        Taa::TsxDisabled
    } else {
        clear_cpu_buf = true;
        if md_clear && !(arch & ARCH_CAP_MDS_NO != 0 && arch & ARCH_CAP_TSX_CTRL_MSR == 0) {
            Taa::Verw
        } else {
            Taa::UcodeNeeded
        }
    };
    let mmio = if !bugs.mmio_stale_data || bugs.mmio_unknown {
        Mmio::Off
    } else {
        if bugs.mds || (bugs.taa && rtm) {
            clear_cpu_buf = true;
        }
        let flush_l1d = edx7 & CPUID_7_0_EDX_FLUSH_L1D != 0;
        if arch & ARCH_CAP_FB_CLEAR != 0 || (md_clear && flush_l1d && arch & ARCH_CAP_MDS_NO == 0) {
            Mmio::Verw
        } else {
            Mmio::UcodeNeeded
        }
    };
    let rfds = if !bugs.rfds {
        Rfds::Off
    } else if arch & ARCH_CAP_RFDS_CLEAR != 0 {
        clear_cpu_buf = true;
        Rfds::Verw
    } else {
        Rfds::UcodeNeeded
    };

    let srbds = bugs.srbds.then_some(
        if arch & ARCH_CAP_MDS_NO != 0 && !rtm && !bugs.mmio_stale_data {
            Srbds::TsxOff
        } else if hypervisor {
            Srbds::Hypervisor
        } else if edx7 & CPUID_7_0_EDX_SRBDS_CTRL == 0 {
            Srbds::UcodeNeeded
        } else {
            Srbds::Full
        },
    );

    let gds = bugs.gds.then(|| {
        if hypervisor {
            Gds::Hypervisor
        } else if arch & ARCH_CAP_GDS_CTRL == 0 {
            Gds::UcodeNeeded
        } else if f.mcu_opt_ctrl.expect("asserted above with GDS_CTRL") & GDS_MITG_LOCKED != 0 {
            Gds::FullLocked
        } else {
            Gds::Full
        }
    });

    // `srso_select_mitigation` (`bugs.c:2670-2807`) under its default
    // `SRSO_CMD_SAFE_RET`; `retbleed` is never `IBPB` here.
    let srso = bugs.srso.then_some(
        if ibpb_brtype && id.family < 0x19 && !f.smt {
            Srso::SmtDisabled
        } else if eax21 & CPUID_8000_0021_EAX_SRSO_USER_KERNEL_NO != 0 {
            if eax21 & CPUID_8000_0021_EAX_SRSO_BP_SPEC_REDUCE != 0 {
                Srso::BpSpecReduce
            } else if ibpb_brtype {
                Srso::IbpbOnVmexit
            } else {
                Srso::UcodeNeeded
            }
        } else if ibpb_brtype {
            Srso::SafeRet
        } else {
            Srso::SafeRetUcodeNeeded
        },
    );
    let sbpb = !bugs.srso && sbpb;

    let its = if bugs.its && spectre_v2 != SpectreV2::None { Its::AlignedThunks } else { Its::Off };

    // `tsa_select_mitigation` (`bugs.c:2918-2962`) and `tsa_init`'s
    // `VERW_CLEAR` (`amd.c:517-530`).
    let tsa = bugs.tsa.then(|| {
        let verw_clear = eax21 & CPUID_8000_0021_EAX_VERW_CLEAR != 0
            || (!hypervisor && table::tsa_microcode(&id, f.microcode));
        if verw_clear {
            clear_cpu_buf = true;
            Tsa::Full
        } else {
            clear_cpu_buf |= hypervisor;
            Tsa::UcodeNeeded
        }
    });

    let vmscape = if !(bugs.vmscape && caps.ibpb) {
        Vmscape::None
    } else if srso == Some(Srso::IbpbOnVmexit) {
        Vmscape::IbpbOnVmexit
    } else {
        Vmscape::IbpbExitToUser
    };

    Ok(Decision {
        tsx,
        spectre_v2,
        bhi,
        ibpb,
        ibrs_fw,
        stibp,
        retbleed,
        ssb,
        mds,
        taa,
        mmio,
        rfds,
        srbds,
        gds,
        its,
        vmscape,
        srso,
        tsa,
        sbpb,
        clear_cpu_buf,
        efer_autoibrs,
        spec_ctrl,
        bugs,
        hypervisor,
        autoibrs: caps.autoibrs,
        smt: f.smt,
    })
}

impl Decision {
    /// The file's line, without its newline.
    pub fn line(&self, v: Vuln) -> Result<Line<'_>, Unmodelled> {
        match v {
            Vuln::L1tf if self.bugs.l1tf => Err(Unmodelled::L1tf),
            Vuln::ItlbMultihit if self.bugs.itlb_multihit => Err(Unmodelled::ItlbMultihit),
            _ => Ok(Line { d: self, v }),
        }
    }
}

/// One vulnerabilities file's line.
pub struct Line<'a> {
    d: &'a Decision,
    v: Vuln,
}

impl Line<'_> {
    fn smt(&self) -> &'static str {
        if self.d.smt { "vulnerable" } else { "disabled" }
    }

    /// `spectre_v2_show_state` (`bugs.c:3238-3257`); unprivileged eBPF is off
    /// under `CONFIG_BPF_UNPRIV_DEFAULT_OFF=y` and no module is loaded.
    fn spectre_v2(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.d;
        f.write_str(match d.spectre_v2 {
            SpectreV2::None => "Vulnerable",
            SpectreV2::Retpoline => "Mitigation: Retpolines",
            SpectreV2::Eibrs => "Mitigation: Enhanced / Automatic IBRS",
            SpectreV2::Ibrs => "Mitigation: IBRS",
        })?;
        f.write_str(match d.ibpb {
            Ibpb::Absent => "",
            Ibpb::Disabled => "; IBPB: disabled",
            Ibpb::Conditional => "; IBPB: conditional",
        })?;
        if d.ibrs_fw {
            f.write_str("; IBRS_FW")?;
        }
        // `stibp_state` (`bugs.c:3174-3193`); `Prctl` is selected only with SMT,
        // which is what enables `switch_to_cond_stibp`.
        if !(d.spectre_v2 == SpectreV2::Eibrs && !d.autoibrs) {
            f.write_str(match d.stibp {
                Stibp::None => "; STIBP: disabled",
                Stibp::StrictPreferred => "; STIBP: always-on",
                Stibp::Prctl => "; STIBP: conditional",
            })?;
        }
        // `RSB_CTXSW` (`bugs.c:1741-1789`).
        if matches!(d.spectre_v2, SpectreV2::Retpoline | SpectreV2::Ibrs) {
            f.write_str("; RSB filling")?;
        }
        // `pbrsb_eibrs_state` (`bugs.c:3207-3218`): every mode but `None`
        // sets `RSB_VMEXIT` or `RSB_VMEXIT_LITE`.
        f.write_str(match (d.bugs.eibrs_pbrsb, d.spectre_v2) {
            (false, _) => "; PBRSB-eIBRS: Not affected",
            (true, SpectreV2::None) => "; PBRSB-eIBRS: Vulnerable",
            (true, _) => "; PBRSB-eIBRS: SW sequence",
        })?;
        f.write_str(match d.bhi {
            Bhi::NotAffected => "; BHI: Not affected",
            Bhi::BhiDisS => "; BHI: BHI_DIS_S",
            Bhi::SwLoop => "; BHI: SW loop, KVM: SW loop",
            Bhi::Retpoline => "; BHI: Retpoline",
            Bhi::Vulnerable => "; BHI: Vulnerable",
        })
    }
}

impl fmt::Display for Line<'_> {
    /// `cpu_show_common` (`bugs.c:3305-3377`) and the `*_show_state` it calls.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.d;
        let b = &d.bugs;
        let affected = match self.v {
            Vuln::Meltdown => b.meltdown,
            Vuln::SpectreV1 => b.spectre_v1,
            Vuln::SpectreV2 => b.spectre_v2,
            Vuln::SpecStoreBypass => b.ssb,
            Vuln::L1tf => b.l1tf,
            Vuln::Mds => b.mds,
            Vuln::TsxAsyncAbort => b.taa,
            Vuln::ItlbMultihit => b.itlb_multihit,
            Vuln::Srbds => b.srbds,
            Vuln::MmioStaleData => b.mmio_stale_data || b.mmio_unknown,
            Vuln::Retbleed => b.retbleed,
            Vuln::GatherDataSampling => b.gds,
            Vuln::RegFileDataSampling => b.rfds,
            Vuln::IndirectTargetSelection => b.its,
            Vuln::Vmscape => b.vmscape,
            Vuln::SpecRstackOverflow => b.srso,
            Vuln::Tsa => b.tsa,
        };
        if !affected {
            return f.write_str("Not affected");
        }
        match self.v {
            // `pti_check_boottime_disable` (`arch/x86/mm/pti.c:79-101`) sets PTI
            // wherever Meltdown is; a Xen PV guest is no machine ToyOS boots.
            Vuln::Meltdown => f.write_str("Mitigation: PTI"),
            Vuln::SpectreV1 => {
                f.write_str("Mitigation: usercopy/swapgs barriers and __user pointer sanitization")
            }
            Vuln::SpectreV2 => self.spectre_v2(f),
            Vuln::SpecStoreBypass => f.write_str(match d.ssb {
                Ssb::None => "Vulnerable",
                Ssb::Prctl => "Mitigation: Speculative Store Bypass disabled via prctl",
            }),
            Vuln::Mds => {
                f.write_str(match d.mds {
                    Mds::Off => "Vulnerable",
                    Mds::Full => "Mitigation: Clear CPU buffers",
                    Mds::Vmwerv => "Vulnerable: Clear CPU buffers attempted, no microcode",
                })?;
                if d.hypervisor {
                    f.write_str("; SMT Host state unknown")
                } else if b.msbds_only {
                    let smt = match (d.mds, d.smt) {
                        (Mds::Off, _) => "vulnerable",
                        (_, true) => "mitigated",
                        (_, false) => "disabled",
                    };
                    write!(f, "; SMT {smt}")
                } else {
                    write!(f, "; SMT {}", self.smt())
                }
            }
            Vuln::TsxAsyncAbort => {
                f.write_str(match d.taa {
                    Taa::Off => "Vulnerable",
                    Taa::UcodeNeeded => "Vulnerable: Clear CPU buffers attempted, no microcode",
                    Taa::Verw => "Mitigation: Clear CPU buffers",
                    Taa::TsxDisabled => "Mitigation: TSX disabled",
                })?;
                match d.taa {
                    Taa::Off | Taa::TsxDisabled => Ok(()),
                    _ if d.hypervisor => f.write_str("; SMT Host state unknown"),
                    _ => write!(f, "; SMT {}", self.smt()),
                }
            }
            Vuln::Srbds => f.write_str(match d.srbds.expect("set with the bug") {
                Srbds::UcodeNeeded => "Vulnerable: No microcode",
                Srbds::Full => "Mitigation: Microcode",
                Srbds::TsxOff => "Mitigation: TSX disabled",
                Srbds::Hypervisor => "Unknown: Dependent on hypervisor status",
            }),
            Vuln::MmioStaleData => {
                if b.mmio_unknown {
                    return f.write_str("Unknown: No mitigations");
                }
                f.write_str(match d.mmio {
                    Mmio::Off => return f.write_str("Vulnerable"),
                    Mmio::UcodeNeeded => "Vulnerable: Clear CPU buffers attempted, no microcode",
                    Mmio::Verw => "Mitigation: Clear CPU buffers",
                })?;
                if d.hypervisor {
                    f.write_str("; SMT Host state unknown")
                } else {
                    write!(f, "; SMT {}", self.smt())
                }
            }
            Vuln::Retbleed => match d.retbleed {
                Retbleed::None => f.write_str("Vulnerable"),
                Retbleed::Ibrs => f.write_str("Mitigation: IBRS"),
                Retbleed::Eibrs => f.write_str("Mitigation: Enhanced IBRS"),
                // Selected on AMD alone, so never "on non-AMD based uarch".
                Retbleed::Unret => {
                    let smt = if !d.smt {
                        "disabled"
                    } else if d.stibp == Stibp::StrictPreferred {
                        "enabled with STIBP protection"
                    } else {
                        "vulnerable"
                    };
                    write!(f, "Mitigation: untrained return thunk; SMT {smt}")
                }
            },
            Vuln::GatherDataSampling => f.write_str(match d.gds.expect("set with the bug") {
                Gds::UcodeNeeded => "Vulnerable: No microcode",
                Gds::Full => "Mitigation: Microcode",
                Gds::FullLocked => "Mitigation: Microcode (locked)",
                Gds::Hypervisor => "Unknown: Dependent on hypervisor status",
            }),
            Vuln::RegFileDataSampling => f.write_str(match d.rfds {
                Rfds::Off => "Vulnerable",
                Rfds::Verw => "Mitigation: Clear Register File",
                Rfds::UcodeNeeded => "Vulnerable: No microcode",
            }),
            Vuln::IndirectTargetSelection => f.write_str(match d.its {
                Its::Off => "Vulnerable",
                Its::AlignedThunks => "Mitigation: Aligned branch/return thunks",
            }),
            Vuln::Vmscape => f.write_str(match d.vmscape {
                Vmscape::None => "Vulnerable",
                Vmscape::IbpbExitToUser => "Mitigation: IBPB before exit to userspace",
                Vmscape::IbpbOnVmexit => "Mitigation: IBPB on VMEXIT",
            }),
            Vuln::SpecRstackOverflow => f.write_str(match d.srso.expect("set with the bug") {
                Srso::SmtDisabled => "Mitigation: SMT disabled",
                Srso::UcodeNeeded => "Vulnerable: No microcode",
                Srso::SafeRetUcodeNeeded => "Vulnerable: Safe RET, no microcode",
                Srso::SafeRet => "Mitigation: Safe RET",
                Srso::IbpbOnVmexit => "Mitigation: IBPB on VMEXIT only",
                Srso::BpSpecReduce => "Mitigation: Reduced Speculation",
            }),
            Vuln::Tsa => f.write_str(match d.tsa.expect("set with the bug") {
                Tsa::UcodeNeeded => "Vulnerable: Clear CPU buffers attempted, no microcode",
                Tsa::Full => "Mitigation: Clear CPU buffers",
            }),
            Vuln::L1tf | Vuln::ItlbMultihit => {
                unreachable!("{} is unmodelled wherever affected", self.v.name())
            }
        }
    }
}
