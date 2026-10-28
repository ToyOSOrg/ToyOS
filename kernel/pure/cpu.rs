//! What an x86 CPU's CPUID words say it is and has: its vendor and
//! signature, whether its HWP performance request is declared and the
//! request, which of its model-specific counters exist, and where it sits in
//! the machine's cache tree.
//!
//! Intel's facts are the SDM's (325462-093), AMD's its PPRs'. Where neither
//! states a value, the rule here is ToyOS's own and says so.

#![forbid(unsafe_code)]

use core::fmt;

use crate::sched::topology::CpuKeys;

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
    /// 0 unless CPUID.8000_0000H reaches leaf 8000_0007H.
    pub cpuid_8000_0007_edx: u32,
    /// The CPU reads its package's counters: the boot processor, so a
    /// machine's package counters are one CPU's.
    pub reads_package: bool,
}

/// The counters a CPU has: a counter it lacks is `#GP` to read, and the
/// kernel has no fault fixup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters {
    /// `IA32_APERF` and `IA32_MPERF`.
    pub aperf_mperf: bool,
    /// `MSR_SMI_COUNT` (34H).
    pub smi: bool,
    /// `MSR_CORE_C6_RESIDENCY` and `MSR_CORE_C7_RESIDENCY` (3FDH, 3FEH).
    pub core_residency: bool,
    /// The package's energy counter and its units, by whose registers.
    pub package_energy: Option<Rapl>,
    /// `MSR_PKG_C2_RESIDENCY`, `_C3_`, `_C6_` and `_C7_` (60DH, 3F8H, 3F9H,
    /// 3FAH).
    pub package_residency: bool,
    /// AMD's `CORE_ENERGY_STAT` (C001_029AH).
    pub core_energy: bool,
}

/// Whose registers carry a package's energy and its units, each counting
/// `1/2^ESU` J, ESU bits 12:8 of the unit register, in bits 31:0 of the
/// energy register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rapl {
    /// `MSR_RAPL_POWER_UNIT` (606H) and `MSR_PKG_ENERGY_STATUS` (611H): SDM
    /// Vol. 3B §17.10.1 and §17.10.3, Table 2-20.
    Intel,
    /// `RAPL_PWR_UNIT` (C001_0299H) and `PKG_ENERGY_STAT` (C001_029BH): PPR
    /// 55922, as 55772 and OSRR 56255 give them for Family 17h's other models.
    Amd,
}

/// `CPUID_Fn80000007_EDX[RAPL]`, "Running average power limit" (PPR 55922).
const CPUID_8000_0007_EDX_RAPL: u32 = 1 << 14;

pub fn counters(facts: &CounterFacts) -> Counters {
    let documented = matches!(facts.vendor, Vendor::Intel | Vendor::Amd);
    let id = Ident::new(facts.vendor, facts.signature);
    // ToyOS's rule for every model-specific counter: under a hypervisor the
    // register is its emulation, not the hardware's count it is read for.
    let hardware = facts.cpuid_1_ecx & CPUID_1_ECX_HYPERVISOR == 0;
    let table_2_20 = hardware && in_table_2_20(&id);
    let amd_rapl = hardware && amd_rapl(&id, facts.cpuid_8000_0007_edx);
    let package_energy = if table_2_20 {
        Some(Rapl::Intel)
    } else if amd_rapl {
        Some(Rapl::Amd)
    } else {
        None
    };
    Counters {
        // SDM Vol. 1 CPUID.06H:ECX[0]; PPR 55922 `CPUID_Fn00000006_ECX[EffFreq]`.
        aperf_mperf: documented && facts.cpuid_6_ecx & CPUID_6_ECX_APERFMPERF != 0,
        smi: hardware && counts_smis(&id),
        core_residency: table_2_20,
        package_energy: package_energy.filter(|_| facts.reads_package),
        // Table 2-42 restates 3F8H for 06_66H without a rate.
        package_residency: table_2_20 && id.model != 0x66 && facts.reads_package,
        core_energy: amd_rapl,
    }
}

/// Family 17h's PPRs in hand (55922 for Model 60h, 55772 for Model 20h,
/// OSRR 56255 for Models 00h-2Fh) each enumerate RAPL by
/// [`CPUID_8000_0007_EDX_RAPL`] and define C001_0299H, C001_029AH and
/// C001_029BH alike: 32-bit energy counters in `1/2^ESU` J. PPR 55901, for
/// Family 19h Model 11h, makes the core's counter 64 bits wide and the
/// package's an L3 complex's, so no other family is read by this rule.
fn amd_rapl(id: &Ident, cpuid_8000_0007_edx: u32) -> bool {
    id.vendor == Vendor::Amd && id.family == 0x17 && cpuid_8000_0007_edx & CPUID_8000_0007_EDX_RAPL != 0
}

/// The family 6 DisplayModels SDM Vol. 4 (325462-093) names as supporting
/// Table 2-20, each in its section's opening (§2.11-§2.16 and §2.17). Table
/// 2-20 defines `MSR_CORE_C6_RESIDENCY` and `_C7_` (3FDH, 3FEH) and
/// `MSR_PKG_C2_RESIDENCY`, `_C3_`, `_C6_` and `_C7_` (60DH, 3F8H, 3F9H,
/// 3FAH) as counting at the TSC's rate, the unit [`Counter::CoreC6`] and
/// [`Counter::PackageC2`] promise, and `MSR_RAPL_POWER_UNIT` (606H) and
/// `MSR_PKG_ENERGY_STATUS` (611H) by §17.10. Of the tables that supersede it
/// for these models, Table 2-57 counts 3FDH and 3FEH in units of the P1
/// clock and gives 3F9H and 3FAH no rate, so 06_AFH is left out, its energy
/// with them; Table 2-42 gives 06_66H's 3F8H no rate; and Tables 2-30, 2-32,
/// 2-36 and 2-50 restate 606H's energy unit as §17.10.1 gives it. Table 2-8,
/// which gives 606H another unit, is no listed model's. AMD documents no
/// residency counter in the PPRs in hand.
///
/// [`Counter::CoreC6`]: toyos_abi::counters::Counter::CoreC6
/// [`Counter::PackageC2`]: toyos_abi::counters::Counter::PackageC2
const TABLE_2_20_MODELS: [u32; 36] = [
    0x2a, 0x2d, 0x3a, 0x3e, 0x3c, 0x45, 0x46, 0x3f, 0x3d, 0x47, 0x4f, 0x56,
    0x4e, 0x5e, 0x55, 0x8e, 0x9e, 0x66, 0xa5, 0xa6, 0x7e, 0x8c, 0x8d, 0x6a,
    0x6c, 0x97, 0x9a, 0xba, 0xb7, 0xbf, 0x8f, 0xcf, 0xaa, 0xad, 0xae, 0xbd,
];

fn in_table_2_20(id: &Ident) -> bool {
    id.vendor == Vendor::Intel && id.family == 6 && TABLE_2_20_MODELS.contains(&id.model)
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

/// Why a CPU's topology leaves describe no place in a tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TopologyRefusal {
    /// Neither CPUID.1FH nor CPUID.0BH enumerates a level.
    NoLevels,
    /// The levels name no SMT level (type 1).
    NoSmtLevel,
    /// The last-level cache is shared by fewer ids than a core holds, or by
    /// more than the package does.
    CacheOutsidePackage { smt_shift: u32, llc_shift: u32, package_shift: u32 },
}

/// One CPU's [`CpuKeys`] from the topology leaves it read of itself, each
/// list running from subleaf 0 to the first invalid subleaf, which ends it
/// wherever it appears.
///
/// `levels` is CPUID.1FH where the CPU has it and CPUID.0BH otherwise (SDM
/// Vol. 2A, CPUID): EAX\[4:0\] the shift to the next level's id, ECX\[15:8\]
/// the level type, 0 past the last level, and EDX the x2APIC id, on every
/// subleaf. The SMT level's shift gives the core and the last level's the
/// package.
///
/// `caches` is the deterministic cache leaf, CPUID.04H on Intel and
/// CPUID.8000_001DH on AMD (PPR 55922, `CPUID_Fn8000001D`), one layout:
/// EAX\[4:0\] the type, 0 past the last cache, 1 data and 3 unified;
/// EAX\[7:5\] the level; EAX\[25:14\] one less than the ids that share it. The
/// highest data or unified cache is the LLC, keyed by the id shifted by the
/// width of that count rounded up to a power of two. A CPU that describes no
/// cache is given one LLC per package: the package is the only shared domain
/// it names.
pub fn topology_keys(levels: &[[u32; 4]], caches: &[[u32; 4]]) -> Result<CpuKeys, TopologyRefusal> {
    let level_type = |q: &[u32; 4]| (q[2] >> 8) & 0xff;
    let shift = |q: &[u32; 4]| q[0] & 0x1f;
    let levels: &[[u32; 4]] = &levels[..levels.iter().position(|q| level_type(q) == 0).unwrap_or(levels.len())];
    let last = levels.last().ok_or(TopologyRefusal::NoLevels)?;
    let x2apic_id = last[3];
    let smt_shift = shift(levels.iter().find(|q| level_type(q) == 1).ok_or(TopologyRefusal::NoSmtLevel)?);
    let package_shift = shift(last);
    let cache_type = |q: &[u32; 4]| q[0] & 0x1f;
    let llc_shift = caches
        .iter()
        .take_while(|q| cache_type(q) != 0)
        .filter(|q| matches!(cache_type(q), 1 | 3))
        .max_by_key(|q| (q[0] >> 5) & 7)
        .map_or(package_shift, |q| (((q[0] >> 14) & 0xfff) + 1).next_power_of_two().trailing_zeros());
    if !(smt_shift..=package_shift).contains(&llc_shift) {
        return Err(TopologyRefusal::CacheOutsidePackage { smt_shift, llc_shift, package_shift });
    }
    Ok(CpuKeys { core: x2apic_id >> smt_shift, llc: x2apic_id >> llc_shift, package: x2apic_id >> package_shift })
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
        CounterFacts {
            vendor: Vendor::Intel,
            signature,
            cpuid_1_ecx: 0,
            cpuid_6_ecx: CPUID_6_ECX_APERFMPERF,
            cpuid_8000_0007_edx: 1 << 8,
            reads_package: true,
        }
    }

    const NONE: Counters = Counters {
        aperf_mperf: false,
        smi: false,
        core_residency: false,
        package_energy: None,
        package_residency: false,
        core_energy: false,
    };

    /// 06_8CH, which §2.17 names as supporting Table 2-20.
    #[test]
    fn the_t14_counts_smis_residency_and_its_packages_energy() {
        let all = Counters {
            aperf_mperf: true,
            smi: true,
            core_residency: true,
            package_energy: Some(Rapl::Intel),
            package_residency: true,
            core_energy: false,
        };
        assert_eq!(counters(&intel(T14_SIGNATURE)), all);
        let ap = CounterFacts { reads_package: false, ..intel(T14_SIGNATURE) };
        assert_eq!(counters(&ap), Counters { package_energy: None, package_residency: false, ..all });
    }

    /// 06_AFH: Table 2-57 counts its core residency at P1, not the TSC.
    #[test]
    fn sierra_forest_counts_no_residency_and_no_energy() {
        let c = counters(&intel(0x000a_06f0));
        assert!(!c.core_residency && !c.package_residency && c.package_energy.is_none());
        assert!(c.smi, "its 34H stands");
    }

    /// 06_66H: Table 2-42 gives its 3F8H no rate, and supersedes nothing else
    /// the rule reads.
    #[test]
    fn cannon_lake_counts_no_package_residency_and_the_rest() {
        let c = counters(&intel(model(0x66)));
        assert!(c.core_residency && c.package_energy == Some(Rapl::Intel) && !c.package_residency);
    }

    /// The AMD bench, Family 17h Model 60h: PPR 55922 gives it `EffFreq`,
    /// `CPUID_Fn80000007_EDX[RAPL]` and no `MSR0000_0034`; without the RAPL
    /// bit it has no energy counter.
    #[test]
    fn the_amd_bench_has_aperf_and_mperf_its_energy_and_no_smi_count() {
        let renoir = CounterFacts {
            vendor: Vendor::Amd,
            signature: 0x0086_0f01,
            cpuid_1_ecx: 0,
            cpuid_6_ecx: 1,
            cpuid_8000_0007_edx: 0x6799,
            reads_package: true,
        };
        let all = Counters { aperf_mperf: true, package_energy: Some(Rapl::Amd), core_energy: true, ..NONE };
        assert_eq!(counters(&renoir), all);
        assert_eq!(counters(&CounterFacts { reads_package: false, ..renoir }), Counters { package_energy: None, ..all });
        assert_eq!(counters(&CounterFacts { cpuid_8000_0007_edx: 0x2799, ..renoir }), Counters { aperf_mperf: true, ..NONE });
    }

    /// PPR 55901 changes the registers' widths and scope for Family 19h.
    #[test]
    fn only_amd_family_17h_reads_rapl() {
        let zen4 = CounterFacts {
            vendor: Vendor::Amd,
            signature: 0x00a1_0f11,
            cpuid_1_ecx: 0,
            cpuid_6_ecx: 1,
            cpuid_8000_0007_edx: 0x6799,
            reads_package: true,
        };
        assert_eq!(counters(&zen4), Counters { aperf_mperf: true, ..NONE });
        let hygon = CounterFacts { vendor: Vendor::Other(*b"HygonGenuine"), signature: 0x0090_0f01, ..zen4 };
        assert_eq!(counters(&hygon), NONE);
    }

    #[test]
    fn a_vendor_neither_intel_nor_amd_has_neither() {
        let other = CounterFacts { vendor: Vendor::Other(*b"CentaurHauls"), ..intel(T14_SIGNATURE) };
        assert_eq!(counters(&other), NONE);
    }

    #[test]
    fn a_guest_counts_nothing_model_specific() {
        let guest = CounterFacts { cpuid_1_ecx: CPUID_1_ECX_HYPERVISOR, ..intel(T14_SIGNATURE) };
        assert_eq!(counters(&guest), Counters { aperf_mperf: true, ..NONE });
        let amd = CounterFacts {
            vendor: Vendor::Amd,
            signature: 0x0086_0f01,
            cpuid_1_ecx: CPUID_1_ECX_HYPERVISOR,
            cpuid_6_ecx: 1,
            cpuid_8000_0007_edx: 0x6799,
            reads_package: true,
        };
        assert_eq!(counters(&amd), Counters { aperf_mperf: true, ..NONE });
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

    /// The T14's cpu4 (x2APIC id 1) and the AMD bench's cpu2 (APIC id 2), each
    /// the words its scout logged (`kernel/sim/tests/scout/`), its invalid
    /// subleaf last.
    #[test]
    fn a_cpus_keys_are_its_id_under_its_own_leaves() {
        let t14_levels = [[0x1, 0x2, 0x100, 0x1], [0x4, 0x8, 0x201, 0x1], [0x0, 0x0, 0x2, 0x1]];
        let t14_caches = [
            [0x1c004121, 0x2c0003f, 0x3f, 0],
            [0x1c004122, 0x1c0003f, 0x3f, 0],
            [0x1c004143, 0x4c0003f, 0x3ff, 0],
            [0x1c03c163, 0x1c0003f, 0x3fff, 4],
            [0, 0, 0, 0],
        ];
        assert_eq!(topology_keys(&t14_levels, &t14_caches), Ok(CpuKeys { core: 0, llc: 0, package: 0 }));
        let amd_levels = [[0x1, 0x2, 0x100, 0x2], [0x7, 0x10, 0x201, 0x2], [0x0, 0x0, 0x2, 0x2]];
        let amd_caches = [
            [0x4121, 0x1c0003f, 0x3f, 0],
            [0x4122, 0x1c0003f, 0x3f, 0],
            [0x4143, 0x1c0003f, 0x3ff, 2],
            [0x1c163, 0x3c0003f, 0xfff, 1],
            [0, 0, 0, 0],
        ];
        assert_eq!(topology_keys(&amd_levels, &amd_caches), Ok(CpuKeys { core: 1, llc: 0, package: 0 }));
        let ccx1 = [[0x1, 0x2, 0x100, 0x9], [0x7, 0x10, 0x201, 0x9]];
        assert_eq!(topology_keys(&ccx1, &amd_caches), Ok(CpuKeys { core: 4, llc: 1, package: 0 }));
    }

    #[test]
    fn a_cpu_that_describes_no_cache_has_one_llc_per_package() {
        let levels = [[0x1, 0x2, 0x100, 0x13], [0x4, 0x8, 0x201, 0x13]];
        assert_eq!(topology_keys(&levels, &[]), Ok(CpuKeys { core: 9, llc: 1, package: 1 }));
        let after_the_last = [[0, 0, 0, 0], [0x1c163, 0, 0, 0]];
        assert_eq!(topology_keys(&levels, &after_the_last), Ok(CpuKeys { core: 9, llc: 1, package: 1 }));
    }

    #[test]
    fn leaves_that_place_no_cpu_are_refused_by_name() {
        assert_eq!(topology_keys(&[], &[]), Err(TopologyRefusal::NoLevels));
        assert_eq!(topology_keys(&[[0, 0, 0x2, 0]], &[]), Err(TopologyRefusal::NoLevels));
        assert_eq!(topology_keys(&[[0x4, 0x8, 0x201, 0]], &[]), Err(TopologyRefusal::NoSmtLevel));
        // A cache 32 ids wide on a package of 16.
        let levels = [[0x1, 0x2, 0x100, 0], [0x4, 0x10, 0x201, 0]];
        assert_eq!(
            topology_keys(&levels, &[[0x7c163, 0, 0, 0]]),
            Err(TopologyRefusal::CacheOutsidePackage { smt_shift: 1, llc_shift: 5, package_shift: 4 }),
        );
    }
}
