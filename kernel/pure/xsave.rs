//! Which state components `XCR0` enables, which instruction saves them and how
//! large the save area is, decided from a CPU's CPUID words (SDM Vol. 1 §13).
//!
//! x87, SSE and AVX are required; the AVX-512 group (opmask, ZMM_Hi256,
//! Hi16_ZMM) is taken whole where the CPU enumerates all of it. Nothing else
//! is enabled: MPX is deprecated, PKRU needs `CR4.PKE`, and AMX and later
//! components each want a decision of their own.
//!
//! The save is the best [`Form`] the CPU offers soundly: `XSAVES`, then
//! `XSAVEC`, then `XSAVEOPT`; a CPU without `XSAVEOPT` is refused, and one
//! AMD erratum 1386 may name ([`erratum_1386`]) never saves with `XSAVES`. The two
//! compacted forms size the area by the SDM's compaction rule over the CPU's
//! own component sizes, which must equal the CPU's `CPUID.(0DH,1):EBX`; the
//! standard form takes `CPUID.(0DH,0):EBX`. Nothing here addresses a component.

#![forbid(unsafe_code)]

use crate::cpu::{Ident, Vendor};

/// `XCR0` bits, SDM Vol. 1 §13.1.
pub const X87: u64 = 1 << 0;
pub const SSE: u64 = 1 << 1;
pub const AVX: u64 = 1 << 2;
pub const OPMASK: u64 = 1 << 5;
pub const ZMM_HI256: u64 = 1 << 6;
pub const HI16_ZMM: u64 = 1 << 7;

/// The components every CPU this kernel runs on must enable.
pub const REQUIRED: u64 = X87 | SSE | AVX;

/// AVX-512's three components, which `XCR0` takes together or not at all.
pub const AVX512: u64 = OPMASK | ZMM_HI256 | HI16_ZMM;

/// The legacy region and the header, where every compacted component's
/// offset starts counting (SDM Vol. 1 §13.4.3).
pub const COMPACTED_BASE: u32 = 576;

/// The largest standard-form area any accepted CPU may report: the end of
/// Hi16_ZMM, which the T14 reports for `XCR0` `0xe7`. The kernel's
/// standard-form first-entry image is this long.
pub const IMAGE_BYTES: usize = 2688;

/// `CPUID.(0DH,1):EAX` bits, SDM Vol. 2A Table 3-8.
const D1_XSAVEOPT: u32 = 1 << 0;
const D1_XSAVEC: u32 = 1 << 1;
const D1_XSAVES: u32 = 1 << 3;

/// The instruction pair that saves and restores user state on every CPU of a
/// machine. The discriminants are what `arch::entry`'s stubs compare against.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    /// `XSAVES` and `XRSTORS`, compacted.
    Xsaves = 1,
    /// `XSAVEC` and `XRSTOR`, compacted.
    Xsavec = 2,
    /// `XSAVEOPT` and `XRSTOR`, standard.
    Xsaveopt = 3,
}

impl Form {
    pub fn compacted(self) -> bool {
        matches!(self, Self::Xsaves | Self::Xsavec)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Xsaves => "xsaves",
            Self::Xsavec => "xsavec",
            Self::Xsaveopt => "xsaveopt",
        }
    }
}

/// What a CPU reports about its extended state.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    /// `CPUID.01H:ECX`: bit 26 is `XSAVE`.
    pub cpuid_1_ecx: u32,
    /// `CPUID.(0DH,0):EAX` and `EDX`: the `XCR0` bits this CPU supports, low
    /// and high. Zero where leaf 0DH does not exist.
    pub cpuid_d0_eax: u32,
    pub cpuid_d0_edx: u32,
    /// `CPUID.(0DH,1):EAX`: `XSAVEOPT`, `XSAVEC` and `XSAVES` at bits 0, 1, 3.
    pub cpuid_d1_eax: u32,
}

/// One state component as `CPUID.(0DH,i)` describes it.
#[derive(Clone, Copy, Debug)]
pub struct Component {
    /// `EAX`: its size in bytes.
    pub size: u32,
    /// `ECX[1]`: in the compacted form it starts on a 64-byte boundary.
    pub aligned: bool,
}

impl Component {
    pub fn from_cpuid(eax: u32, ecx: u32) -> Self {
        Self { size: eax, aligned: ecx & 2 != 0 }
    }
}

/// Why a CPU cannot run this kernel's user state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    NoXsave,
    NoXsaveopt,
    /// A required component the CPU does not support, as the `XCR0` bits missing.
    Missing(u64),
    /// Some of the AVX-512 group and not all, as the bits supported.
    PartialAvx512(u64),
    /// The standard area the CPU reports for the declared `XCR0`, past
    /// [`IMAGE_BYTES`].
    AreaTooLarge(u32),
    /// The compacted size the SDM's rule gives over the CPU's own components,
    /// and the one the CPU reports in `CPUID.(0DH,1):EBX`.
    CompactedSize { computed: u32, reported: u32 },
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoXsave => "it has no XSAVE (CPUID.01H:ECX[26])",
            Self::NoXsaveopt => "it has no XSAVEOPT (CPUID.(0DH,1):EAX[0]), the least save this kernel takes",
            Self::Missing(_) => "it does not support x87, SSE and AVX state in XCR0",
            Self::PartialAvx512(_) => {
                "it enumerates part of the AVX-512 state group, which XCR0 takes whole or not at all"
            }
            Self::AreaTooLarge(_) => "its standard save area is larger than the first-entry image",
            Self::CompactedSize { .. } => {
                "its compacted save area is not the size its components give by SDM Vol. 1 §13.4.3"
            }
        }
    }
}

/// The `XCR0` this CPU is declared with.
pub fn xcr0(facts: &Facts) -> Result<u64, Refusal> {
    if facts.cpuid_1_ecx & (1 << 26) == 0 {
        return Err(Refusal::NoXsave);
    }
    let supported = (facts.cpuid_d0_edx as u64) << 32 | facts.cpuid_d0_eax as u64;
    let missing = REQUIRED & !supported;
    if missing != 0 {
        return Err(Refusal::Missing(missing));
    }
    match supported & AVX512 {
        0 => Ok(REQUIRED),
        AVX512 => Ok(REQUIRED | AVX512),
        partial => Err(Refusal::PartialAvx512(partial)),
    }
}

/// The save the CPU `id` takes. One [`erratum_1386`] names saves with
/// `XSAVEC`, which saves what `XSAVES` would while `IA32_XSS` is 0.
pub fn form(facts: &Facts, id: &Ident) -> Result<Form, Refusal> {
    let d1 = facts.cpuid_d1_eax;
    if d1 & D1_XSAVEOPT == 0 {
        return Err(Refusal::NoXsaveopt);
    }
    Ok(if d1 & D1_XSAVES != 0 && !erratum_1386(id) {
        Form::Xsaves
    } else if d1 & D1_XSAVEC != 0 {
        Form::Xsavec
    } else {
        Form::Xsaveopt
    })
}

/// `(family, model, stepping)` of the revisions a public AMD revision guide
/// lists without erratum 1386: 55449 rev 1.21's ZP-B1 and PiR-B2, and 57095
/// rev 1.01's Genoa-B1.
const CLEARED_1386: [(u32, u32, u32); 3] = [(0x17, 0x01, 0x1), (0x17, 0x08, 0x2), (0x19, 0x11, 0x1)];

/// Whether AMD erratum 1386, "XSAVES Instruction May Fail to Save XMM
/// Registers to the Provided State Save Area" ("No fix planned"), may name the
/// CPU: AMD's guides list it against ZP-B2 (55449), Rome-B0 (56323) and
/// Milan-B1 and -B2 (56683). Every other AMD family 17h and 19h part, and a
/// family 18h part of any vendor but Intel, is taken as named unless a guide
/// clears it: a wrong yes costs only `XSAVES`' modified optimisation, a wrong
/// no hands a thread an older XMM value of its own.
/// `issues/amd-erratum-1386-is-assumed-where-no-public-amd-revision-guide-clears-a-part.md`
/// owns that assumption.
pub fn erratum_1386(id: &Ident) -> bool {
    let named = match id.vendor {
        Vendor::Amd => (0x17..=0x19).contains(&id.family),
        Vendor::Other(_) => id.family == 0x18,
        Vendor::Intel => false,
    };
    named && !CLEARED_1386.contains(&(id.family, id.model, id.stepping))
}

/// The bytes one standard-form save writes, from `CPUID.(0DH,0):EBX` read once
/// the declared `XCR0` is in place, and no more than [`IMAGE_BYTES`].
pub fn standard_bytes(cpuid_d0_ebx: u32) -> Result<usize, Refusal> {
    match cpuid_d0_ebx as usize {
        bytes if bytes > IMAGE_BYTES => Err(Refusal::AreaTooLarge(cpuid_d0_ebx)),
        bytes => Ok(bytes),
    }
}

/// The compacted area for `enabled`, by SDM Vol. 1 §13.4.3: the components
/// past SSE in ascending order from [`COMPACTED_BASE`], each on the next
/// 64-byte boundary where it asks for one.
pub fn compacted_size(enabled: u64, component: impl Fn(u32) -> Component) -> u32 {
    (2..63).filter(|&i| enabled & (1 << i) != 0).fold(COMPACTED_BASE, |end, i| {
        let c = component(i);
        let at = if c.aligned { end.next_multiple_of(64) } else { end };
        at + c.size
    })
}

/// The bytes one compacted save of `enabled` writes: [`compacted_size`], which
/// must be what the CPU reports in `CPUID.(0DH,1):EBX` with `IA32_XSS` clear.
pub fn compacted_bytes(enabled: u64, component: impl Fn(u32) -> Component, cpuid_d1_ebx: u32) -> Result<usize, Refusal> {
    match compacted_size(enabled, component) {
        computed if computed == cpuid_d1_ebx => Ok(computed as usize),
        computed => Err(Refusal::CompactedSize { computed, reported: cpuid_d1_ebx }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// The T14's own `fpu:` line at its boot, with `XCR0` at `0xe7` and
    /// `IA32_XSS` at 0 (`control_regs::self_check` asserts both).
    const T14_FPU: &str = "fpu: cpu0 cpuid.1.eax=0x806c1 xsave=1 osxsave=1 xcr0=0xe7 cpuid.d.0=(0x2e7,2688,2696) \
        cpuid.d.1=(0xf,2432) cpuid.d.2..9.(eax,ebx,ecx)=[(256, 576, 0), (0, 0, 0), (0, 0, 0), (64, 1088, 0), \
        (512, 1152, 0), (1024, 1664, 0), (0, 0, 0), (0, 0, 0)] save=xsaves area=2432";

    /// Every number in [`T14_FPU`] after `key`, in order, to the next field.
    fn t14(key: &str) -> Vec<u32> {
        let rest = T14_FPU.split_once(key).expect("a field the line carries").1;
        let field = rest.split(" save=").next().expect("the line ends in save=");
        let field = if key.starts_with("cpuid.d.2") { field } else { field.split(' ').next().expect("a word") };
        field
            .split(|c: char| !c.is_ascii_hexdigit() && c != 'x')
            .filter(|w| !w.is_empty())
            .map(|w| match w.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16).expect("a hex number"),
                None => w.parse().expect("a decimal number"),
            })
            .collect()
    }

    /// `CPUID.(0DH,0):EDX`, `XCR0`'s upper half, is not on the line; no
    /// component this kernel enables lies there.
    fn t14_facts() -> Facts {
        Facts {
            cpuid_1_ecx: if t14("xsave=") == [1] { 1 << 26 } else { 0 },
            cpuid_d0_eax: t14("cpuid.d.0=")[0],
            cpuid_d0_edx: 0,
            cpuid_d1_eax: t14("cpuid.d.1=")[0],
        }
    }

    /// Component `i`'s `(size, offset, ecx)` as the T14 reported it.
    fn t14_component(i: u32) -> Component {
        let words = t14("cpuid.d.2..9.(eax,ebx,ecx)=");
        let at = (i as usize - 2) * 3;
        Component::from_cpuid(words[at], words[at + 2])
    }

    fn t14_ident() -> Ident {
        Ident::new(Vendor::Intel, t14("cpuid.1.eax=")[0])
    }

    /// The AMD bench, a Ryzen 7 4800U: leaf 0DH as this kernel logged it
    /// there (`cpuid.d.0=(0x207,…)`, `cpuid.d.1.eax=0xf`).
    fn amd_bench() -> Facts {
        Facts { cpuid_1_ecx: 1 << 26, cpuid_d0_eax: 0x207, cpuid_d0_edx: 0, cpuid_d1_eax: 0xf }
    }

    /// TCG's `qemu64` with `Arch::cpu`'s flags: leaf 0DH as this kernel
    /// logged it there (`cpuid.d.0=(0x7,…)`, `cpuid.d.1.eax=0x1`).
    fn tcg() -> Facts {
        Facts { cpuid_1_ecx: 1 << 26, cpuid_d0_eax: 0x7, cpuid_d0_edx: 0, cpuid_d1_eax: 0x1 }
    }

    #[test]
    fn the_t14_declares_avx512_and_nothing_past_it() {
        // Its CPUID offers PKRU (bit 9) as well; PKRU is not this declaration's.
        assert_eq!(t14_facts().cpuid_d0_eax & (1 << 9), 1 << 9);
        assert_eq!(xcr0(&t14_facts()), Ok(0xe7));
    }

    #[test]
    fn the_amd_bench_and_tcg_declare_avx() {
        assert_eq!(xcr0(&amd_bench()), Ok(0x7));
        assert_eq!(xcr0(&tcg()), Ok(0x7));
    }

    #[test]
    fn the_t14_saves_with_xsaves() {
        assert_eq!(form(&t14_facts(), &t14_ident()), Ok(Form::Xsaves));
    }

    /// Its CPUID enumerates `XSAVES`; the erratum is what moves it.
    #[test]
    fn the_amd_bench_named_by_the_erratum_saves_with_xsavec() {
        assert_eq!(amd_bench().cpuid_d1_eax & D1_XSAVES, D1_XSAVES);
        assert_eq!(form(&amd_bench(), &Ident::new(Vendor::Amd, 0x0086_0f01)), Ok(Form::Xsavec));
    }

    #[test]
    fn tcg_saves_with_xsaveopt() {
        assert_eq!(form(&tcg(), &Ident::new(Vendor::Amd, 0x0006_0fb1)), Ok(Form::Xsaveopt));
    }

    #[test]
    fn a_cpu_without_xsave_is_refused() {
        let facts = Facts { cpuid_1_ecx: t14_facts().cpuid_1_ecx & !(1 << 26), ..t14_facts() };
        assert_eq!(xcr0(&facts), Err(Refusal::NoXsave));
    }

    #[test]
    fn a_cpu_without_xsaveopt_is_refused_whatever_else_it_offers() {
        let facts = Facts { cpuid_d1_eax: D1_XSAVES | D1_XSAVEC, ..t14_facts() };
        assert_eq!(form(&facts, &t14_ident()), Err(Refusal::NoXsaveopt));
    }

    #[test]
    fn a_part_the_erratum_names_without_xsavec_saves_with_xsaveopt() {
        let facts = Facts { cpuid_d1_eax: D1_XSAVEOPT | D1_XSAVES, ..amd_bench() };
        assert_eq!(form(&facts, &Ident::new(Vendor::Amd, 0x0086_0f01)), Ok(Form::Xsaveopt));
    }

    fn amd(signature: u32) -> bool {
        erratum_1386(&Ident::new(Vendor::Amd, signature))
    }

    /// Each revision AMD's guides list it against, by the raw signature
    /// their CPUID tables give.
    #[test]
    fn each_revision_amds_guides_list_the_erratum_against_is_named() {
        for signature in [0x0080_0f12, 0x0083_0f10, 0x00a0_0f11, 0x00a0_0f12] {
            assert!(amd(signature), "{signature:#x}");
        }
    }

    /// ZP-B1 and PiR-B2 (55449), and Genoa-B1, whose guide (57095) does not
    /// list it.
    #[test]
    fn the_revisions_a_guide_lists_without_it_are_cleared() {
        for signature in [0x0080_0f11, 0x0080_0f82, 0x00a1_0f11] {
            assert!(!amd(signature), "{signature:#x}");
        }
    }

    /// The AMD bench, family 17h model 60h, which no public guide covers,
    /// every other family 17h and 19h model, and family 18h of any vendor but
    /// Intel.
    #[test]
    fn a_part_no_guide_clears_is_named() {
        for signature in [0x0086_0f01, 0x0080_0ff0, 0x008b_0f00, 0x00a1_0f12, 0x00af_0ff0] {
            assert!(amd(signature), "{signature:#x}");
        }
        assert!(amd(0x0090_0f02));
        assert!(erratum_1386(&Ident::new(Vendor::Other(*b"HygonGenuine"), 0x0090_0f02)));
    }

    /// The T14, TCG's `qemu64` (AMD family 0FH), family 1AH, Intel at any
    /// signature, and another vendor's family 17h.
    #[test]
    fn no_other_part_is_named() {
        assert!(!erratum_1386(&t14_ident()));
        assert!(!erratum_1386(&Ident::new(Vendor::Intel, 0x0080_0f12)));
        assert!(!amd(0x0006_0fb1));
        assert!(!amd(0x00b0_0f20));
        assert!(!erratum_1386(&Ident::new(Vendor::Other(*b"CentaurHauls"), 0x0080_0f12)));
    }

    #[test]
    fn a_cpu_without_avx_state_is_refused() {
        let facts = Facts { cpuid_d0_eax: 0x3, ..amd_bench() };
        assert_eq!(xcr0(&facts), Err(Refusal::Missing(AVX)));
    }

    #[test]
    fn part_of_the_avx512_group_is_refused() {
        for partial in [OPMASK, ZMM_HI256, HI16_ZMM, OPMASK | HI16_ZMM, ZMM_HI256 | HI16_ZMM] {
            let facts = Facts { cpuid_d0_eax: 0x7 | partial as u32, ..amd_bench() };
            assert_eq!(xcr0(&facts), Err(Refusal::PartialAvx512(partial)), "{partial:#x}");
        }
    }

    #[test]
    fn a_standard_area_up_to_the_first_entry_image_is_taken_as_reported() {
        // SDM Vol. 1 §13.4: 512 legacy bytes and the 64-byte header, then
        // YMM_Hi128's 256 bytes at offset 576.
        assert_eq!(standard_bytes(832), Ok(832));
        assert_eq!(standard_bytes(IMAGE_BYTES as u32), Ok(IMAGE_BYTES));
    }

    #[test]
    fn a_standard_area_past_the_first_entry_image_is_refused() {
        // The T14's own figure with every component it supports, PKRU among
        // them, which this kernel never declares.
        let with_pkru = t14("cpuid.d.0=")[2];
        assert_eq!(with_pkru, IMAGE_BYTES as u32 + 8);
        assert_eq!(standard_bytes(with_pkru), Err(Refusal::AreaTooLarge(with_pkru)));
    }

    /// Each component's size by SDM Vol. 1 §13.1-§13.5, none aligned.
    fn sdm(i: u32) -> Component {
        let size = match i {
            2 => 256,
            5 => 64,
            6 => 512,
            7 => 1024,
            9 => 8,
            11 => 16,
            _ => panic!("component {i} is not one these tests size"),
        };
        Component { size, aligned: false }
    }

    /// The oracle: the T14's own `CPUID.(0DH,1):EBX` at `XCR0` `0xe7` and
    /// `IA32_XSS` 0, 2432, is what the SDM's rule gives over the sizes it
    /// reported, and over the SDM's own.
    #[test]
    fn the_sdm_rule_gives_the_t14s_reported_compacted_size() {
        let reported = t14("cpuid.d.1=")[1];
        assert_eq!(t14("xcr0=")[0], 0xe7);
        assert_eq!(reported, 2432);
        assert_eq!(compacted_bytes(0xe7, t14_component, reported), Ok(2432));
        assert_eq!(compacted_bytes(0xe7, sdm, reported), Ok(2432));
    }

    /// Hi16_ZMM's end at the T14's offsets is the standard area it reports
    /// for `0xe7`, and the image's length.
    #[test]
    fn the_first_entry_image_is_the_t14s_standard_area() {
        let hi16_zmm = t14_component(7);
        assert_eq!(t14("cpuid.d.2..9.(eax,ebx,ecx)=")[5 * 3 + 1] + hi16_zmm.size, t14("cpuid.d.0=")[1]);
        assert_eq!(t14("cpuid.d.0=")[1] as usize, IMAGE_BYTES);
    }

    #[test]
    fn the_declared_sets_compact_to_their_component_sums() {
        assert_eq!(compacted_size(0x7, sdm), 832);
        assert_eq!(compacted_size(0xe7, sdm), 2432);
    }

    #[test]
    fn an_aligned_component_starts_on_the_next_64_byte_boundary() {
        // PKRU ends at 2440; an aligned CET_U starts at 2496, not 2440.
        let aligned = |i| Component { aligned: i == 11, ..sdm(i) };
        assert_eq!(compacted_size(0x2e7 | 1 << 11, aligned), 2512);
    }

    #[test]
    fn a_compacted_size_the_cpu_does_not_report_is_refused() {
        assert_eq!(
            compacted_bytes(0xe7, sdm, 2440),
            Err(Refusal::CompactedSize { computed: 2432, reported: 2440 }),
        );
    }

    #[test]
    fn components_from_cpuid_take_ecx_bit_1_as_alignment() {
        assert!(Component::from_cpuid(64, 2).aligned);
        assert!(!Component::from_cpuid(64, 1).aligned);
    }
}
