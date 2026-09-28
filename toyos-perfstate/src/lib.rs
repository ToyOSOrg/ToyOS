//! The CPU performance request this kernel declares on an x86-64 CPU with
//! hardware-controlled performance states (HWP), and the register layouts a
//! reader checks it against. Pure: the kernel supplies CPUID and the MSR reads
//! and does the writes.
//!
//! Layouts are the Intel SDM's — Vol. 3B, *Power and Thermal Management*, for
//! HWP, the energy/performance bias and package thermal status; Vol. 4 for the
//! addresses. The declared values are the power envelope the self-hosting bar
//! is measured under (`issues/build/toyos-builds-itself.md`), so a ToyOS run
//! and the Linux run it is held against ask the CPU for the same thing.

#![no_std]
#![forbid(unsafe_code)]

/// MSR addresses, SDM Vol. 4.
pub mod msr {
    /// `IA32_PM_ENABLE`: bit 0 enables HWP, and only a reset clears it.
    pub const PM_ENABLE: u32 = 0x770;
    pub const HWP_CAPABILITIES: u32 = 0x771;
    pub const HWP_REQUEST_PKG: u32 = 0x772;
    /// Exists where CPUID.06H:EAX[8] says so ([`super::hwp_notification`]).
    pub const HWP_INTERRUPT: u32 = 0x773;
    pub const HWP_REQUEST: u32 = 0x774;
    pub const MISC_ENABLE: u32 = 0x1A0;
    pub const ENERGY_PERF_BIAS: u32 = 0x1B0;
    pub const PACKAGE_THERM_STATUS: u32 = 0x1B1;
    /// `MSR_PLATFORM_INFO`: enumerated by no CPUID bit. Vol. 4 documents it in
    /// the model tables of DisplayFamily 06H, which [`super::refusal`] requires.
    pub const PLATFORM_INFO: u32 = 0xCE;
}

/// `IA32_MISC_ENABLE` bit 38: set, turbo is off.
pub const TURBO_DISABLE: u64 = 1 << 38;

/// `IA32_PM_ENABLE` on every CPU.
pub const PM_ENABLE: u64 = 1;

/// `IA32_HWP_INTERRUPT` on every CPU that has it: no HWP notification is
/// enabled, as intel_pstate leaves it — nothing in this kernel takes one.
pub const HWP_INTERRUPT: u64 = 0;

/// `IA32_ENERGY_PERF_BIAS` on every CPU: the bar's 6, on the scale where 0 is
/// performance and 15 is energy saving.
pub const ENERGY_PERF_BIAS: u64 = 6;

/// The energy/performance preference every CPU's request carries: the bar's
/// 128, which is Linux's `balance_performance` on the bar's machine.
pub const EPP: u8 = 128;

/// `IA32_HWP_REQUEST_PKG`: the bar's value. Inert — no CPU's request sets
/// package control — and declared so that firmware does not decide it either.
pub const HWP_REQUEST_PKG: u64 =
    HwpRequest { min: 1, max: 255, desired: 0, epp: EPP, window: 0, package_control: false }.raw();

/// What CPUID says about the registers the envelope names.
#[derive(Clone, Copy, Debug)]
pub struct Cpuid {
    /// Leaf 0's `EBX`, `EDX`, `ECX`, in that order: the vendor string.
    pub vendor: [u8; 12],
    pub max_leaf: u32,
    pub leaf1_eax: u32,
    pub leaf6_eax: u32,
    pub leaf6_ecx: u32,
    pub leaf7_edx: u32,
}

/// Why a CPU gets no declared performance request, and its performance state
/// stays whatever firmware left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NoLeaf6,
    NoHwp,
    NotIntel,
    NotFamily6,
    NoEpp,
    NoPackageRequest,
    NoEnergyPerfBias,
    NoPackageThermal,
    Hybrid,
}

impl Refusal {
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoLeaf6 => "CPUID has no leaf 6, so no power management is enumerated",
            Self::NoHwp => "no HWP (CPUID.06H:EAX[7] clear)",
            Self::NotIntel => {
                "HWP on a CPU that is not Intel, whose RAPL and thermal registers are Intel's \
                 model-specific ones"
            }
            Self::NotFamily6 => {
                "an Intel CPU outside DisplayFamily 06H, for which the SDM documents no \
                 MSR_PLATFORM_INFO (0xCE), and no CPUID bit enumerates it"
            }
            Self::NoEpp => "HWP without an energy/performance preference (CPUID.06H:EAX[10] clear)",
            Self::NoPackageRequest => "HWP without a package-level request (CPUID.06H:EAX[11] clear)",
            Self::NoEnergyPerfBias => "no energy/performance bias (CPUID.06H:ECX[3] clear)",
            Self::NoPackageThermal => "no package thermal status (CPUID.06H:EAX[6] clear)",
            Self::Hybrid => {
                "a hybrid CPU (CPUID.07H:EDX[15]), whose HWP scale is not its ratio scale, \
                 and the declared minimum is a ratio"
            }
        }
    }
}

/// `None` where every register the envelope names exists on this CPU.
/// **All or nothing**: a CPU missing one gets no request at all, so no reader
/// ever takes firmware's value for one register as the declaration's.
pub const fn refusal(cpuid: &Cpuid) -> Option<Refusal> {
    const PTM: u32 = 1 << 6;
    const HWP: u32 = 1 << 7;
    const HWP_EPP: u32 = 1 << 10;
    const HWP_PKG: u32 = 1 << 11;
    const EPB: u32 = 1 << 3;
    const HYBRID: u32 = 1 << 15;
    let eax = cpuid.leaf6_eax;
    if cpuid.max_leaf < 6 {
        Some(Refusal::NoLeaf6)
    } else if eax & HWP == 0 {
        Some(Refusal::NoHwp)
    } else if !is_intel(&cpuid.vendor) {
        Some(Refusal::NotIntel)
    } else if display_family(cpuid.leaf1_eax) != 6 {
        Some(Refusal::NotFamily6)
    } else if eax & HWP_EPP == 0 {
        Some(Refusal::NoEpp)
    } else if eax & HWP_PKG == 0 {
        Some(Refusal::NoPackageRequest)
    } else if cpuid.leaf6_ecx & EPB == 0 {
        Some(Refusal::NoEnergyPerfBias)
    } else if eax & PTM == 0 {
        Some(Refusal::NoPackageThermal)
    } else if cpuid.max_leaf >= 7 && cpuid.leaf7_edx & HYBRID != 0 {
        Some(Refusal::Hybrid)
    } else {
        None
    }
}

/// Whether this CPU has `IA32_HWP_INTERRUPT` (CPUID.06H:EAX[8]).
pub const fn hwp_notification(cpuid: &Cpuid) -> bool {
    cpuid.leaf6_eax & 1 << 8 != 0
}

/// CPUID.01H:EAX's DisplayFamily (SDM Vol. 2A, CPUID): the extended family
/// counts only where the family field is 0FH.
const fn display_family(leaf1_eax: u32) -> u32 {
    let family = (leaf1_eax >> 8) & 0xf;
    if family == 0xf {
        family + ((leaf1_eax >> 20) & 0xff)
    } else {
        family
    }
}

const fn is_intel(vendor: &[u8; 12]) -> bool {
    let want = b"GenuineIntel";
    let mut i = 0;
    while i < want.len() {
        if vendor[i] != want[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `IA32_HWP_REQUEST` for a CPU whose `IA32_HWP_CAPABILITIES` reads
/// `capabilities`, in a package whose `MSR_PLATFORM_INFO` reads
/// `platform_info`. The bar names every field: the minimum is the package's
/// maximum-efficiency ratio, the maximum is the CPU's highest performance —
/// turbo included, the capabilities' bits 7:0 — and the CPU chooses between
/// them, unwindowed, on its own.
pub const fn hwp_request(capabilities: u64, platform_info: u64) -> u64 {
    HwpRequest {
        min: ((platform_info >> 40) & 0xff) as u8,
        max: capabilities as u8,
        desired: 0,
        epp: EPP,
        window: 0,
        package_control: false,
    }
    .raw()
}

/// `IA32_HWP_REQUEST`'s fields; `IA32_HWP_REQUEST_PKG` is the same layout
/// with bit 42 reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwpRequest {
    pub min: u8,
    pub max: u8,
    /// 0 leaves the choice to the CPU.
    pub desired: u8,
    pub epp: u8,
    /// Bits 41:32, ten bits wide; 0 leaves the window to the CPU.
    pub window: u16,
    /// Set, this CPU follows the package request instead.
    pub package_control: bool,
}

impl HwpRequest {
    pub const fn raw(self) -> u64 {
        self.min as u64
            | (self.max as u64) << 8
            | (self.desired as u64) << 16
            | (self.epp as u64) << 24
            | ((self.window & 0x3ff) as u64) << 32
            | (self.package_control as u64) << 42
    }

    /// The fields; bits above 42 are not one of them.
    pub const fn of(raw: u64) -> Self {
        Self {
            min: raw as u8,
            max: (raw >> 8) as u8,
            desired: (raw >> 16) as u8,
            epp: (raw >> 24) as u8,
            window: ((raw >> 32) & 0x3ff) as u16,
            package_control: raw & 1 << 42 != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `IA32_HWP_CAPABILITIES` the T14's i5-1135G7 reads under Linux: its
    /// cores differ in the most efficient level and agree in the rest.
    const T14_CAPABILITIES: [u64; 2] = [0x010d_182a, 0x010e_182a];
    /// `MSR_PLATFORM_INFO` bits 47:40 on the T14: Linux's `cpuinfo_min_freq`
    /// there is 400000 kHz, which intel_pstate computes as that ratio times
    /// 100 MHz. The register's other bits are not the declaration's business.
    const T14_PLATFORM_INFO: u64 = 4 << 40;
    /// What every one of the T14's eight CPUs held under Linux, intel_pstate
    /// active with EPP `balance_performance`.
    const T14_REQUEST: u64 = 0x8000_2a04;
    /// CPUID.01H:EAX's family field at 6, every other field 0.
    const FAMILY_6: u32 = 6 << 8;

    fn intel(leaf6_eax: u32, leaf6_ecx: u32, leaf7_edx: u32) -> Cpuid {
        Cpuid {
            vendor: *b"GenuineIntel",
            max_leaf: 0x1b,
            leaf1_eax: FAMILY_6,
            leaf6_eax,
            leaf6_ecx,
            leaf7_edx,
        }
    }

    /// The bits a CPU needs: PTM, HWP, EPP and the package request in `EAX`,
    /// EPB in `ECX`.
    const EAX: u32 = 1 << 6 | 1 << 7 | 1 << 10 | 1 << 11;
    const ECX: u32 = 1 << 3;

    #[test]
    fn the_t14_is_asked_for_what_linux_asked_for() {
        for caps in T14_CAPABILITIES {
            assert_eq!(hwp_request(caps, T14_PLATFORM_INFO), T14_REQUEST, "{caps:#x}");
        }
    }

    /// The maximum is the capabilities' highest level alone: the three levels
    /// above it in the register are not read into the request.
    #[test]
    fn the_maximum_is_the_highest_level_alone() {
        assert_eq!(HwpRequest::of(hwp_request(0xffff_ff2a, 0)).max, 42);
        assert_eq!(HwpRequest::of(hwp_request(0x0000_0000, 0)).max, 0);
    }

    #[test]
    fn the_package_request_is_the_bars() {
        assert_eq!(HWP_REQUEST_PKG, 0x8000_ff01);
    }

    #[test]
    fn the_t14s_request_decodes_to_the_bars_fields() {
        assert_eq!(
            HwpRequest::of(T14_REQUEST),
            HwpRequest { min: 4, max: 42, desired: 0, epp: 128, window: 0, package_control: false },
        );
    }

    /// Each field alone, at the bit the SDM gives it — so two fields swapped
    /// with equal values in the T14's case cannot hide.
    #[test]
    fn every_field_is_where_the_sdm_puts_it() {
        let zero = HwpRequest { min: 0, max: 0, desired: 0, epp: 0, window: 0, package_control: false };
        assert_eq!(HwpRequest { min: 0xff, ..zero }.raw(), 0xff);
        assert_eq!(HwpRequest { max: 0xff, ..zero }.raw(), 0xff << 8);
        assert_eq!(HwpRequest { desired: 0xff, ..zero }.raw(), 0xff << 16);
        assert_eq!(HwpRequest { epp: 0xff, ..zero }.raw(), 0xff << 24);
        assert_eq!(HwpRequest { window: 0x3ff, ..zero }.raw(), 0x3ff << 32);
        assert_eq!(HwpRequest { window: 0xffff, ..zero }.raw(), 0x3ff << 32);
        assert_eq!(HwpRequest { package_control: true, ..zero }.raw(), 1 << 42);
        let all = HwpRequest { min: 1, max: 2, desired: 3, epp: 4, window: 5, package_control: true };
        assert_eq!(HwpRequest::of(all.raw()), all);
    }

    #[test]
    fn a_cpu_with_every_register_is_declared() {
        assert_eq!(refusal(&intel(EAX, ECX, 0)), None);
    }

    /// QEMU's two x86-64 CPUs: TCG's `qemu64` says AMD and leaf 6 has only
    /// `ARAT` (bit 2); KVM's `host` passes the vendor and the same leaf 6.
    #[test]
    fn no_qemu_cpu_is_declared() {
        let tcg = Cpuid { vendor: *b"AuthenticAMD", max_leaf: 0xd, ..intel(1 << 2, 0, 0) };
        assert_eq!(refusal(&tcg), Some(Refusal::NoHwp));
        assert_eq!(refusal(&intel(1 << 2, 0, 0)), Some(Refusal::NoHwp));
    }

    #[test]
    fn a_missing_register_refuses_the_whole_request_by_name() {
        let without = |eax: u32| refusal(&intel(EAX & !eax, ECX, 0));
        assert_eq!(without(1 << 10), Some(Refusal::NoEpp));
        assert_eq!(without(1 << 11), Some(Refusal::NoPackageRequest));
        assert_eq!(without(1 << 6), Some(Refusal::NoPackageThermal));
        assert_eq!(refusal(&intel(EAX, 0, 0)), Some(Refusal::NoEnergyPerfBias));
        assert_eq!(refusal(&intel(EAX, ECX, 1 << 15)), Some(Refusal::Hybrid));
        let old = Cpuid { max_leaf: 5, ..intel(EAX, ECX, 0) };
        assert_eq!(refusal(&old), Some(Refusal::NoLeaf6));
        let amd = Cpuid { vendor: *b"AuthenticAMD", ..intel(EAX, ECX, 0) };
        assert_eq!(refusal(&amd), Some(Refusal::NotIntel));
        // Leaf 7 above the maximum is not read: a stale `EDX` there says nothing.
        let six = Cpuid { max_leaf: 6, ..intel(EAX, ECX, 1 << 15) };
        assert_eq!(refusal(&six), None);
    }

    /// `MSR_PLATFORM_INFO` is the family's, so a CPU of any other family is
    /// refused before the register is read — the extended family included,
    /// which is 0FH plus its field and never its field alone.
    #[test]
    fn a_cpu_outside_family_6_is_refused_by_name() {
        let of = |leaf1_eax: u32| refusal(&Cpuid { leaf1_eax, ..intel(EAX, ECX, 0) });
        assert_eq!(of(FAMILY_6), None);
        // Family 0FH, extended family 0: a Pentium 4.
        assert_eq!(of(0x0000_0f41), Some(Refusal::NotFamily6));
        // Family 0FH, extended family 3: DisplayFamily 12H.
        assert_eq!(of(0x0030_0f00), Some(Refusal::NotFamily6));
        // An extended family beside family 6 does not count.
        assert_eq!(of(0x0ff0_0600), None);
    }

    #[test]
    fn notification_is_bit_8_alone() {
        assert!(hwp_notification(&intel(EAX | 1 << 8, ECX, 0)));
        assert!(!hwp_notification(&intel(EAX, ECX, 0)));
        assert!(!hwp_notification(&intel(!(1 << 8), ECX, 0)));
    }
}
