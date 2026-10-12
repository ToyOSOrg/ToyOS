//! Which state components `XCR0` enables, which instruction saves them and how
//! large the save area is, decided from a CPU's CPUID words (SDM Vol. 1 §13).
//!
//! x87, SSE and AVX are required; the AVX-512 group (opmask, ZMM_Hi256,
//! Hi16_ZMM) is taken whole where the CPU enumerates all of it. Nothing else
//! is enabled: MPX is deprecated, PKRU needs `CR4.PKE`, and AMX and later
//! components each want a decision of their own.
//!
//! The save is the best [`Form`] the CPU offers soundly: `XSAVES`, then
//! `XSAVEC`, then `XSAVEOPT`; a CPU without `XSAVEOPT` is refused. The two
//! compacted forms size the area by the SDM's compaction rule over the CPU's
//! own component sizes, which must equal the CPU's `CPUID.(0DH,1):EBX`; the
//! standard form takes `CPUID.(0DH,0):EBX`. Nothing here addresses a component.

#![forbid(unsafe_code)]

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
/// Hi16_ZMM (`0xa88` on the T14 with PKRU beside it,
/// `kernel/cpuvuln/fixtures/t14/cpuid.txt`, less PKRU's 8 bytes). The
/// kernel's standard-form first-entry image is this long.
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

/// The save this CPU takes. `erratum_1386` is `toyos_cpuvuln::erratum_1386`'s
/// verdict on it: a CPU it names saves with `XSAVEC`, which saves what
/// `XSAVES` would while `IA32_XSS` is 0.
pub fn form(facts: &Facts, erratum_1386: bool) -> Result<Form, Refusal> {
    let d1 = facts.cpuid_d1_eax;
    if d1 & D1_XSAVEOPT == 0 {
        return Err(Refusal::NoXsaveopt);
    }
    Ok(if d1 & D1_XSAVES != 0 && !erratum_1386 {
        Form::Xsaves
    } else if d1 & D1_XSAVEC != 0 {
        Form::Xsavec
    } else {
        Form::Xsaveopt
    })
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

    /// `(leaf, subleaf) -> [eax, ebx, ecx, edx]` off the T14's recorded CPUID,
    /// whose provenance is `kernel/cpuvuln/fixtures/t14/SOURCE`.
    fn t14(leaf: u32, subleaf: u32) -> [u32; 4] {
        let rows: Vec<[u32; 6]> = include_str!("../cpuvuln/fixtures/t14/cpuid.txt")
            .lines()
            .map(|line| {
                let words: Vec<u32> =
                    line.split_whitespace().map(|w| u32::from_str_radix(w, 16).expect("a hex word")).collect();
                words.try_into().expect("six words a line")
            })
            .collect();
        let row = rows.iter().find(|r| r[0] == leaf && r[1] == subleaf).expect("the leaf is recorded");
        [row[2], row[3], row[4], row[5]]
    }

    fn t14_facts() -> Facts {
        Facts {
            cpuid_1_ecx: t14(1, 0)[2],
            cpuid_d0_eax: t14(0xd, 0)[0],
            cpuid_d0_edx: t14(0xd, 0)[3],
            cpuid_d1_eax: t14(0xd, 1)[0],
        }
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
        assert_eq!(form(&t14_facts(), false), Ok(Form::Xsaves));
    }

    /// Its CPUID enumerates `XSAVES`; the erratum is what moves it.
    #[test]
    fn the_amd_bench_named_by_the_erratum_saves_with_xsavec() {
        assert_eq!(amd_bench().cpuid_d1_eax & D1_XSAVES, D1_XSAVES);
        assert_eq!(form(&amd_bench(), true), Ok(Form::Xsavec));
    }

    #[test]
    fn tcg_saves_with_xsaveopt() {
        assert_eq!(form(&tcg(), false), Ok(Form::Xsaveopt));
    }

    #[test]
    fn a_cpu_without_xsave_is_refused() {
        let facts = Facts { cpuid_1_ecx: t14_facts().cpuid_1_ecx & !(1 << 26), ..t14_facts() };
        assert_eq!(xcr0(&facts), Err(Refusal::NoXsave));
    }

    #[test]
    fn a_cpu_without_xsaveopt_is_refused_whatever_else_it_offers() {
        let facts = Facts { cpuid_d1_eax: D1_XSAVES | D1_XSAVEC, ..t14_facts() };
        assert_eq!(form(&facts, false), Err(Refusal::NoXsaveopt));
    }

    #[test]
    fn a_part_the_erratum_names_without_xsavec_saves_with_xsaveopt() {
        let facts = Facts { cpuid_d1_eax: D1_XSAVEOPT | D1_XSAVES, ..amd_bench() };
        assert_eq!(form(&facts, true), Ok(Form::Xsaveopt));
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
        // The T14's own figure with PKRU in XCR0, which this kernel never declares.
        let with_pkru = t14(0xd, 0)[1];
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

    /// The oracle: the T14's recorded `CPUID.(0DH,1):EBX`, `0x998`, read under
    /// its Linux with `XCR0` = `0x2e7` and, by that kernel's configuration,
    /// CET_U (bit 11) in `IA32_XSS`, is what the SDM's rule gives that set.
    #[test]
    fn the_sdm_rule_gives_the_t14s_recorded_compacted_size() {
        assert_eq!(t14(0xd, 1)[1], 0x998);
        assert_eq!(compacted_bytes(0x2e7 | 1 << 11, sdm, t14(0xd, 1)[1]), Ok(0x998));
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
