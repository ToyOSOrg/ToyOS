//! Whether a CPU's performance request is declared through HWP, and the
//! request: the one `intel_pstate` asks of it under the `powersave` governor
//! at its defaults, read at upstream Linux `v6.8`'s
//! `drivers/cpufreq/intel_pstate.c` and `arch/x86/include/asm/msr-index.h`,
//! not at this crate's pinned tag. The minimum is `MSR_PLATFORM_INFO[47:40]`
//! (`core_get_min_pstate`, `:1859-1864`), the maximum the CPU's highest level,
//! turbo included (`__intel_pstate_get_hwp_cap`, `:940-948`), and the
//! preference `HWP_EPP_BALANCE_PERFORMANCE` (`msr-index.h:500`), the
//! `balance_performance` it leaves on every CPU `intel_epp_balance_perf`
//! (`:3410-3419`) does not list.
//!
//! **The refusals are ToyOS's, not Linux's**: `intel_pstate` drives hybrid
//! CPUs on a scale of their own and leaves other vendors to other drivers,
//! and neither is modelled here.

use crate::{Ident, Vendor};

/// What one CPU says about itself, read on that CPU.
#[derive(Clone, Copy, Debug)]
pub struct HwpFacts {
    pub vendor: Vendor,
    /// CPUID.1:EAX.
    pub signature: u32,
    /// 0 unless CPUID.0:EAX reaches 6.
    pub cpuid_6_eax: u32,
    /// 0 unless CPUID.0:EAX reaches 6.
    pub cpuid_6_ecx: u32,
    /// 0 unless CPUID.0:EAX reaches 7.
    pub cpuid_7_0_edx: u32,
}

/// A CPU whose request is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hwp {
    /// `IA32_HWP_INTERRUPT` exists (CPUID.6:EAX[8]); it is `#GP` where not.
    pub notify: bool,
}

/// Why a CPU's request is not declared, and its performance state stays what
/// firmware left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwpRefusal {
    NoHwp,
    NotIntel,
    NotFamily6,
    NoEpp,
    NoPackageRequest,
    NoEnergyPerfBias,
    Hybrid,
}

impl HwpRefusal {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoHwp => "no HWP (CPUID.6:EAX[7])",
            Self::NotIntel => "HWP on a CPU that is not Intel's, and the minimum is MSR_PLATFORM_INFO's, which is Intel's",
            Self::NotFamily6 => "an Intel CPU outside family 6, which no CPUID bit enumerates MSR_PLATFORM_INFO on",
            Self::NoEpp => "HWP without an energy/performance preference (CPUID.6:EAX[10])",
            Self::NoPackageRequest => "HWP without a package request (CPUID.6:EAX[11])",
            Self::NoEnergyPerfBias => "no energy/performance bias (CPUID.6:ECX[3])",
            Self::Hybrid => "a hybrid CPU (CPUID.(7,0):EDX[15]), whose HWP scale is not its ratio scale",
        }
    }
}

// `arch/x86/include/asm/cpufeatures.h`'s bits at their CPUID positions.
const CPUID_6_EAX_HWP: u32 = 1 << 7;
const CPUID_6_EAX_HWP_NOTIFY: u32 = 1 << 8;
const CPUID_6_EAX_HWP_EPP: u32 = 1 << 10;
const CPUID_6_EAX_HWP_PKG_REQ: u32 = 1 << 11;
const CPUID_6_ECX_EPB: u32 = 1 << 3;
const CPUID_7_0_EDX_HYBRID_CPU: u32 = 1 << 15;

/// **All or nothing**: a CPU lacking one register the declaration writes gets
/// none of them, so no reader takes firmware's value of one for the
/// declaration's.
pub fn hwp(facts: &HwpFacts) -> Result<Hwp, HwpRefusal> {
    let eax = facts.cpuid_6_eax;
    let refusal = if eax & CPUID_6_EAX_HWP == 0 {
        Some(HwpRefusal::NoHwp)
    } else if facts.vendor != Vendor::Intel {
        Some(HwpRefusal::NotIntel)
    } else if Ident::new(facts.vendor, facts.signature).family != 6 {
        Some(HwpRefusal::NotFamily6)
    } else if eax & CPUID_6_EAX_HWP_EPP == 0 {
        Some(HwpRefusal::NoEpp)
    } else if eax & CPUID_6_EAX_HWP_PKG_REQ == 0 {
        Some(HwpRefusal::NoPackageRequest)
    } else if facts.cpuid_6_ecx & CPUID_6_ECX_EPB == 0 {
        Some(HwpRefusal::NoEnergyPerfBias)
    } else if facts.cpuid_7_0_edx & CPUID_7_0_EDX_HYBRID_CPU != 0 {
        Some(HwpRefusal::Hybrid)
    } else {
        None
    };
    match refusal {
        Some(refusal) => Err(refusal),
        None => Ok(Hwp { notify: eax & CPUID_6_EAX_HWP_NOTIFY != 0 }),
    }
}

/// `HWP_EPP_BALANCE_PERFORMANCE`.
pub const HWP_EPP: u64 = 0x80;

/// `IA32_HWP_REQUEST` for a CPU whose `IA32_HWP_CAPABILITIES` reads
/// `capabilities`, in a package whose `MSR_PLATFORM_INFO` reads
/// `platform_info`: no desired level, no activity window, no package control,
/// so the CPU chooses between the two levels itself (SDM Vol. 3B, "Managing
/// HWP"; `msr-index.h:495-504`).
pub const fn hwp_request(capabilities: u64, platform_info: u64) -> u64 {
    let min = (platform_info >> 40) & 0xff;
    let max = capabilities & 0xff;
    min | max << 8 | HWP_EPP << 24
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    /// The T14's CPU 0, off `fixtures/t14/cpuid.txt`.
    fn t14() -> HwpFacts {
        let leaves: Vec<Vec<u32>> = include_str!("../fixtures/t14/cpuid.txt")
            .lines()
            .map(|l| l.split(' ').map(|w| u32::from_str_radix(w, 16).expect(w)).collect())
            .collect();
        let leaf = |n: u32| leaves.iter().find(|l| l[..2] == [n, 0]).expect("a leaf the capture read");
        let id: Vec<u8> = [leaf(0)[3], leaf(0)[5], leaf(0)[4]].iter().flat_map(|r| r.to_le_bytes()).collect();
        assert!(leaf(0)[2] >= 7, "the T14 reaches leaf 7");
        HwpFacts {
            vendor: Vendor::from_id(id[..].try_into().unwrap()),
            signature: leaf(1)[2],
            cpuid_6_eax: leaf(6)[2],
            cpuid_6_ecx: leaf(6)[4],
            cpuid_7_0_edx: leaf(7)[5],
        }
    }

    /// `cpuinfo.txt` lists `hwp hwp_notify hwp_act_window hwp_epp
    /// hwp_pkg_req` and `epb`, and no `hybrid_cpu`.
    #[test]
    fn the_t14_is_declared_with_its_notification() {
        let flags = include_str!("../fixtures/t14/cpuinfo.txt")
            .lines()
            .find_map(|l| l.strip_prefix("flags\t\t: "))
            .expect("a flags line");
        for flag in ["hwp", "hwp_notify", "hwp_epp", "hwp_pkg_req", "epb"] {
            assert!(flags.split(' ').any(|f| f == flag), "{flag}");
        }
        assert!(!flags.split(' ').any(|f| f == "hybrid_cpu"));
        assert_eq!(hwp(&t14()), Ok(Hwp { notify: true }));
        assert_eq!(hwp(&HwpFacts { cpuid_6_eax: t14().cpuid_6_eax & !CPUID_6_EAX_HWP_NOTIFY, ..t14() }), Ok(Hwp { notify: false }));
    }

    /// QEMU's `qemu64`, the TCG model: `AuthenticAMD`, and no HWP in leaf 6.
    #[test]
    fn the_tcg_model_is_refused() {
        let tcg = HwpFacts { vendor: Vendor::Amd, signature: 0x0006_0fb1, cpuid_6_eax: 1 << 2, cpuid_6_ecx: 0, cpuid_7_0_edx: 0 };
        assert_eq!(hwp(&tcg), Err(HwpRefusal::NoHwp));
    }

    #[test]
    fn a_missing_register_refuses_the_whole_request_by_name() {
        let without = |eax: u32, ecx: u32| hwp(&HwpFacts { cpuid_6_eax: t14().cpuid_6_eax & !eax, cpuid_6_ecx: t14().cpuid_6_ecx & !ecx, ..t14() });
        assert_eq!(without(CPUID_6_EAX_HWP, 0), Err(HwpRefusal::NoHwp));
        assert_eq!(without(CPUID_6_EAX_HWP_EPP, 0), Err(HwpRefusal::NoEpp));
        assert_eq!(without(CPUID_6_EAX_HWP_PKG_REQ, 0), Err(HwpRefusal::NoPackageRequest));
        assert_eq!(without(0, CPUID_6_ECX_EPB), Err(HwpRefusal::NoEnergyPerfBias));
        assert_eq!(hwp(&HwpFacts { cpuid_7_0_edx: CPUID_7_0_EDX_HYBRID_CPU, ..t14() }), Err(HwpRefusal::Hybrid));
        assert_eq!(hwp(&HwpFacts { vendor: Vendor::Amd, ..t14() }), Err(HwpRefusal::NotIntel));
    }

    /// The family is `x86_family`'s: 0xF plus the extended field, never the
    /// extended field beside family 6.
    #[test]
    fn a_cpu_outside_family_6_is_refused_by_name() {
        let of = |signature: u32| hwp(&HwpFacts { signature, ..t14() });
        assert_eq!(of(0x0000_0f41), Err(HwpRefusal::NotFamily6));
        assert_eq!(of(0x0030_0f00), Err(HwpRefusal::NotFamily6));
        assert_eq!(of(0x0ff0_0600), Ok(Hwp { notify: true }));
    }

    /// Each field where `msr-index.h` puts it, from the bits its source names
    /// and no other.
    #[test]
    fn the_request_is_the_minimum_ratio_the_highest_level_and_the_preference() {
        assert_eq!(hwp_request(0, 0), 0x8000_0000);
        assert_eq!(hwp_request(0xffff_ff2a, 0), 0x8000_2a00);
        assert_eq!(hwp_request(0, 0x04 << 40), 0x8000_0004);
        assert_eq!(hwp_request(0, !(0xff << 40)), 0x8000_0000);
        assert_eq!(hwp_request(u64::MAX, u64::MAX), 0x8000_ffff);
    }
}
