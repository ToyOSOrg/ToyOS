//! What an x86 CPU's CPUID words say it is and has: its vendor and
//! signature, whether its HWP performance request is declared and the
//! request, and which of its model-specific counters exist.
//!
//! Intel's facts are the SDM's (325462-093), AMD's its PPRs'. Where neither
//! states a value, the rule here is ToyOS's own and says so.

#![forbid(unsafe_code)]

use core::fmt;

/// CPUID.0's identification string (EBX, EDX, ECX bytes in that order):
/// `"GenuineIntel"` (SDM Vol. 1 §21.1.1) and `"AuthenticAMD"` (PPR 55922,
/// `CPUID_Fn00000000`). Every other string is `Other`: ToyOS reads no other
/// vendor's feature flags.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    Other([u8; 12]),
}

impl Vendor {
    pub const fn from_id(id: &[u8; 12]) -> Self {
        match id {
            b"GenuineIntel" => Self::Intel,
            b"AuthenticAMD" => Self::Amd,
            _ => Self::Other(*id),
        }
    }
}

impl fmt::Debug for Vendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Intel => f.write_str("Intel"),
            Self::Amd => f.write_str("Amd"),
            Self::Other(id) => {
                f.write_str("Other(\"")?;
                for &b in id {
                    if b.is_ascii_graphic() || b == b' ' {
                        write!(f, "{}", b as char)?;
                    } else {
                        write!(f, "\\x{b:02x}")?;
                    }
                }
                f.write_str("\")")
            }
        }
    }
}

/// A CPU's vendor and its CPUID.01H:EAX signature decoded by SDM Vol. 1
/// §21.1.2's DisplayFamily/DisplayModel rule, which PPR 55922's
/// `CPUID_Fn00000001_EAX` states for AMD too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ident {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
}

impl Ident {
    pub const fn new(vendor: Vendor, signature: u32) -> Self {
        let base_family = (signature >> 8) & 0xf;
        let base_model = (signature >> 4) & 0xf;
        let family = if base_family == 0xf { base_family + ((signature >> 20) & 0xff) } else { base_family };
        let model = if base_family == 0x6 || base_family == 0xf {
            ((signature >> 16) & 0xf) << 4 | base_model
        } else {
            base_model
        };
        Self { vendor, family, model, stepping: signature & 0xf }
    }
}

/// CPUID.01H:ECX[31]: "Not Used … Allocated for use by software emulation"
/// (SDM Vol. 1, CPUID.01H:ECX), the bit a hypervisor sets for its guest
/// (APM Vol. 3 §E.3.2).
pub const CPUID_1_ECX_HYPERVISOR: u32 = 1 << 31;

// CPUID.06H and CPUID.(07H,0) bits, SDM Vol. 1 Table 21 and Vol. 3B §17.4.1.
const CPUID_6_EAX_HWP: u32 = 1 << 7;
const CPUID_6_EAX_HWP_NOTIFY: u32 = 1 << 8;
const CPUID_6_EAX_HWP_EPP: u32 = 1 << 10;
const CPUID_6_EAX_HWP_PKG_REQ: u32 = 1 << 11;
const CPUID_6_ECX_APERFMPERF: u32 = 1 << 0;
const CPUID_6_ECX_EPB: u32 = 1 << 3;
const CPUID_7_0_EDX_HYBRID: u32 = 1 << 15;

/// What one CPU says about its HWP, read on that CPU; a leaf CPUID.0 does
/// not reach reads as zero.
#[derive(Clone, Copy, Debug)]
pub struct HwpFacts {
    pub vendor: Vendor,
    pub cpuid_6_eax: u32,
    pub cpuid_6_ecx: u32,
    pub cpuid_7_0_edx: u32,
}

/// A CPU whose request is declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hwp {
    /// `IA32_HWP_INTERRUPT` exists (CPUID.06H:EAX[8]); it is `#GP` where not.
    pub notify: bool,
}

/// Why a CPU's request is not declared, and its performance state stays what
/// firmware left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwpRefusal {
    NoHwp,
    NotIntel,
    NoEpp,
    NoPackageRequest,
    NoEnergyPerfBias,
    Hybrid,
}

impl HwpRefusal {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoHwp => "no HWP (CPUID.06H:EAX[7])",
            Self::NotIntel => "HWP's CPUID bits on a CPU that is not Intel's, where SDM Vol. 1 §21.1.1 gives them no meaning",
            Self::NoEpp => "HWP without an energy/performance preference (CPUID.06H:EAX[10])",
            Self::NoPackageRequest => "HWP without a package request (CPUID.06H:EAX[11])",
            Self::NoEnergyPerfBias => "no energy/performance bias (CPUID.06H:ECX[3])",
            Self::Hybrid => "a hybrid CPU (CPUID.(07H,0):EDX[15]), which no bench ToyOS has can measure the declaration on",
        }
    }
}

/// **All or nothing**: a CPU lacking one register the declaration writes gets
/// none of them, so no reader takes firmware's value of one for the
/// declaration's.
pub fn hwp(facts: &HwpFacts) -> Result<Hwp, HwpRefusal> {
    let eax = facts.cpuid_6_eax;
    if eax & CPUID_6_EAX_HWP == 0 {
        Err(HwpRefusal::NoHwp)
    } else if facts.vendor != Vendor::Intel {
        Err(HwpRefusal::NotIntel)
    } else if eax & CPUID_6_EAX_HWP_EPP == 0 {
        Err(HwpRefusal::NoEpp)
    } else if eax & CPUID_6_EAX_HWP_PKG_REQ == 0 {
        Err(HwpRefusal::NoPackageRequest)
    } else if facts.cpuid_6_ecx & CPUID_6_ECX_EPB == 0 {
        Err(HwpRefusal::NoEnergyPerfBias)
    } else if facts.cpuid_7_0_edx & CPUID_7_0_EDX_HYBRID != 0 {
        Err(HwpRefusal::Hybrid)
    } else {
        Ok(Hwp { notify: eax & CPUID_6_EAX_HWP_NOTIFY != 0 })
    }
}

/// `Energy_Performance_Preference`: 80H, §17.4.4.1's default and the middle
/// of its scale.
pub const HWP_EPP: u64 = 0x80;

/// `IA32_HWP_REQUEST` for a CPU whose `IA32_HWP_CAPABILITIES` reads
/// `capabilities`: SDM Vol. 3B §17.4.4.1's defaults, the minimum
/// `Lowest_Performance` and the maximum `Highest_Performance`, both static
/// (§17.4.3), with [`HWP_EPP`]; no desired level, no activity window and no
/// package control, so the CPU chooses between the two levels itself.
pub const fn hwp_request(capabilities: u64) -> u64 {
    let highest = capabilities & 0xff;
    let lowest = (capabilities >> 24) & 0xff;
    lowest | highest << 8 | HWP_EPP << 24
}

/// What one CPU says about its counters, read on that CPU.
#[derive(Clone, Copy, Debug)]
pub struct CounterFacts {
    pub vendor: Vendor,
    /// CPUID.01H:EAX.
    pub signature: u32,
    pub cpuid_1_ecx: u32,
    /// 0 unless CPUID.0 reaches leaf 6.
    pub cpuid_6_ecx: u32,
}

/// The counters a CPU has: a counter it lacks is `#GP` to read, and the
/// kernel has no fault fixup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters {
    /// `IA32_APERF` and `IA32_MPERF`.
    pub aperf_mperf: bool,
    /// `MSR_SMI_COUNT` (34H).
    pub smi: bool,
}

pub fn counters(facts: &CounterFacts) -> Counters {
    let documented = matches!(facts.vendor, Vendor::Intel | Vendor::Amd);
    Counters {
        // SDM Vol. 1 CPUID.06H:ECX[0]; PPR 55922 `CPUID_Fn00000006_ECX[EffFreq]`.
        aperf_mperf: documented && facts.cpuid_6_ecx & CPUID_6_ECX_APERFMPERF != 0,
        // ToyOS's rule: under a hypervisor 34H is its emulation, not the
        // firmware's count it is read for.
        smi: counts_smis(&Ident::new(facts.vendor, facts.signature))
            && facts.cpuid_1_ecx & CPUID_1_ECX_HYPERVISOR == 0,
    }
}

/// The Intel family 6 DisplayModels a section of SDM Vol. 4 names as
/// supporting a table that carries `MSR_SMI_COUNT`: Table 2-6 (§2.4-§2.7),
/// 2-15 (§2.8-§2.10), 2-20 (§2.11-§2.17) and 2-61 (§2.18). A model Table 2-1
/// lists and no section names is left out: admitting one wrongly is a `#GP`
/// at bring-up, leaving one out loses only its count. AMD documents no 34H.
const SMI_COUNT_MODELS: [u32; 57] = [
    // Table 2-6.
    0x37, 0x4a, 0x4d, 0x5a, 0x5d, 0x4c, 0x5c, 0x7a, 0x86, 0x96, 0x9c,
    // Table 2-15.
    0x1a, 0x1e, 0x1f, 0x2e, 0x25, 0x2c, 0x2f,
    // Table 2-20.
    0x2a, 0x2d, 0x3a, 0x3e, 0x3c, 0x45, 0x46, 0x3f, 0x3d, 0x47, 0x4f, 0x56,
    0x4e, 0x5e, 0x55, 0x8e, 0x9e, 0x66, 0xa5, 0xa6, 0x7e, 0x8c, 0x8d, 0x6a,
    0x6c, 0x97, 0x9a, 0xba, 0xb7, 0xbf, 0x8f, 0xcf, 0xaa, 0xad, 0xae, 0xaf, 0xbd,
    // Table 2-61.
    0x57, 0x85,
];

fn counts_smis(id: &Ident) -> bool {
    id.vendor == Vendor::Intel && id.family == 6 && SMI_COUNT_MODELS.contains(&id.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The T14's `control_regs:` line at its own boot: `hwp_capabilities=0x0104182a`,
    /// Highest 2AH, Most_Efficient 04H, Lowest 01H.
    #[test]
    fn the_t14s_request_is_the_sdms_defaults_over_its_capabilities() {
        assert_eq!(hwp_request(0x0104_182a), 0x8000_2a01);
    }

    /// Each field where §17.4.4.1's Figure 17-6 puts it, from the
    /// capabilities' fields Figure 17-5 names, and from no other bit.
    #[test]
    fn the_request_is_lowest_highest_and_the_preference() {
        assert_eq!(hwp_request(0), 0x8000_0000);
        assert_eq!(hwp_request(0x2a), 0x8000_2a00);
        assert_eq!(hwp_request(0x04 << 24), 0x8000_0004);
        assert_eq!(hwp_request(0x00ff_ff00), 0x8000_0000);
        assert_eq!(hwp_request(u64::MAX), 0x8000_ffff);
    }

    /// The T14's leaf 6 and leaf 7 by the bits its own `control_regs:` lines
    /// prove: HWP, its interrupt, EPP, the package request, EPB, not hybrid.
    fn t14() -> HwpFacts {
        HwpFacts {
            vendor: Vendor::Intel,
            cpuid_6_eax: CPUID_6_EAX_HWP | CPUID_6_EAX_HWP_NOTIFY | CPUID_6_EAX_HWP_EPP | CPUID_6_EAX_HWP_PKG_REQ,
            cpuid_6_ecx: CPUID_6_ECX_EPB | CPUID_6_ECX_APERFMPERF,
            cpuid_7_0_edx: 0,
        }
    }

    #[test]
    fn the_t14_is_declared_with_its_notification() {
        assert_eq!(hwp(&t14()), Ok(Hwp { notify: true }));
        let quiet = HwpFacts { cpuid_6_eax: t14().cpuid_6_eax & !CPUID_6_EAX_HWP_NOTIFY, ..t14() };
        assert_eq!(hwp(&quiet), Ok(Hwp { notify: false }));
    }

    #[test]
    fn a_missing_register_refuses_the_whole_request_by_name() {
        let without = |eax: u32, ecx: u32| {
            hwp(&HwpFacts { cpuid_6_eax: t14().cpuid_6_eax & !eax, cpuid_6_ecx: t14().cpuid_6_ecx & !ecx, ..t14() })
        };
        assert_eq!(without(1 << 7, 0), Err(HwpRefusal::NoHwp));
        assert_eq!(without(1 << 10, 0), Err(HwpRefusal::NoEpp));
        assert_eq!(without(1 << 11, 0), Err(HwpRefusal::NoPackageRequest));
        assert_eq!(without(0, 1 << 3), Err(HwpRefusal::NoEnergyPerfBias));
        assert_eq!(hwp(&HwpFacts { cpuid_7_0_edx: 1 << 15, ..t14() }), Err(HwpRefusal::Hybrid));
    }

    /// PPR 55922's `CPUID_Fn00000006_EAX` reads 4 (ARAT alone), so the AMD
    /// bench has no HWP; and on any vendor but Intel the bits mean nothing.
    #[test]
    fn only_intel_declares_hwp() {
        assert_eq!(hwp(&HwpFacts { vendor: Vendor::Amd, cpuid_6_eax: 0x4, cpuid_6_ecx: 0x1, cpuid_7_0_edx: 0 }), Err(HwpRefusal::NoHwp));
        assert_eq!(hwp(&HwpFacts { vendor: Vendor::Amd, ..t14() }), Err(HwpRefusal::NotIntel));
        assert_eq!(hwp(&HwpFacts { vendor: Vendor::Other(*b"HygonGenuine"), ..t14() }), Err(HwpRefusal::NotIntel));
    }

    const T14_SIGNATURE: u32 = 0x0008_06c1;

    fn intel(signature: u32) -> CounterFacts {
        CounterFacts { vendor: Vendor::Intel, signature, cpuid_1_ecx: 0, cpuid_6_ecx: CPUID_6_ECX_APERFMPERF }
    }

    /// 06_8CH, which §2.17 names as supporting Table 2-20.
    #[test]
    fn the_t14_counts_smis_and_has_aperf_and_mperf() {
        assert_eq!(counters(&intel(T14_SIGNATURE)), Counters { aperf_mperf: true, smi: true });
    }

    /// The AMD bench, Family 17h Model 60h: PPR 55922 gives it `EffFreq` and
    /// no `MSR0000_0034`.
    #[test]
    fn the_amd_bench_has_aperf_and_mperf_and_no_smi_count() {
        let renoir = CounterFacts { vendor: Vendor::Amd, signature: 0x0086_0f01, cpuid_1_ecx: 0, cpuid_6_ecx: 1 };
        assert_eq!(counters(&renoir), Counters { aperf_mperf: true, smi: false });
    }

    #[test]
    fn a_vendor_neither_intel_nor_amd_has_neither() {
        let other = CounterFacts { vendor: Vendor::Other(*b"CentaurHauls"), ..intel(T14_SIGNATURE) };
        assert_eq!(counters(&other), Counters { aperf_mperf: false, smi: false });
    }

    #[test]
    fn a_guest_counts_no_smis() {
        let guest = CounterFacts { cpuid_1_ecx: CPUID_1_ECX_HYPERVISOR, ..intel(T14_SIGNATURE) };
        assert_eq!(counters(&guest), Counters { aperf_mperf: true, smi: false });
    }

    /// Family 6 signatures with the given DisplayModel, stepping 1.
    fn model(m: u32) -> u32 {
        0x0000_0601 | (m >> 4) << 16 | (m & 0xf) << 4
    }

    /// One model from each table's sections, and every model Table 2-1 lists
    /// that no section names (`smi-list.md` in #915's evidence).
    #[test]
    fn a_model_counts_smis_exactly_where_vol_4_names_it() {
        for m in [0x37, 0x4c, 0x9c, 0x1a, 0x2f, 0x2a, 0x3a, 0x66, 0xbd, 0x57, 0x85] {
            assert!(counters(&intel(model(m))).smi, "06_{m:02X}");
        }
        for m in [0x5f, 0xa7, 0xbe, 0xb6, 0xb5, 0xc5, 0xc6, 0xcc, 0xd5, 0xdd] {
            assert!(!counters(&intel(model(m))).smi, "06_{m:02X}");
        }
    }

    #[test]
    fn only_intel_family_6_counts_smis() {
        assert!(!counters(&intel(0x0000_0f41)).smi);
        assert!(!counters(&CounterFacts { vendor: Vendor::Amd, ..intel(T14_SIGNATURE) }).smi);
    }

    /// §21.1.2: the extended model joins family 6 and 0FH, the extended
    /// family 0FH alone.
    #[test]
    fn the_signature_decodes_by_the_display_rule() {
        let t14 = Ident::new(Vendor::Intel, T14_SIGNATURE);
        assert_eq!((t14.family, t14.model, t14.stepping), (6, 0x8c, 1));
        let renoir = Ident::new(Vendor::Amd, 0x0086_0f01);
        assert_eq!((renoir.family, renoir.model, renoir.stepping), (0x17, 0x60, 1));
        let p4 = Ident::new(Vendor::Intel, 0x0000_0f41);
        assert_eq!((p4.family, p4.model), (0xf, 4));
        let pentium = Ident::new(Vendor::Intel, 0x0001_0543);
        assert_eq!((pentium.family, pentium.model), (5, 4));
    }

    #[test]
    fn another_vendors_string_is_kept() {
        let id = *b"  Shanghai  ";
        assert_eq!(Vendor::from_id(&id), Vendor::Other(id));
        assert_eq!(std::format!("{:?}", Vendor::from_id(&id)), "Other(\"  Shanghai  \")");
        assert_eq!(Vendor::from_id(b"GenuineIntel"), Vendor::Intel);
        assert_eq!(Vendor::from_id(b"AuthenticAMD"), Vendor::Amd);
    }
}
