//! Which state components `XCR0` enables and how large the save area for them
//! is, decided from a CPU's CPUID words (SDM Vol. 1 §13).
//!
//! x87, SSE and AVX are required; the AVX-512 group (opmask, ZMM_Hi256,
//! Hi16_ZMM) is taken whole where the CPU enumerates all of it. Nothing else
//! is enabled: MPX is deprecated, PKRU needs `CR4.PKE`, and AMX and later
//! components each want a decision of their own.
//!
//! The area is the standard form's, whose size for the enabled set the CPU
//! reports in `CPUID.(0DH,0):EBX` once `XCR0` holds it; nothing here
//! addresses a component.

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

/// The largest area any accepted CPU may report: the end of Hi16_ZMM in the
/// standard form on the CPUs that enumerate it (`0xa88` on the T14 with PKRU
/// beside it, `kernel/cpuvuln/fixtures/t14/cpuid.txt`, less PKRU's 8 bytes).
/// The kernel's first-entry image is this long.
pub const IMAGE_BYTES: usize = 2688;

/// What a CPU reports about its extended state.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    /// `CPUID.01H:ECX`: bit 26 is `XSAVE`.
    pub cpuid_1_ecx: u32,
    /// `CPUID.(0DH,0):EAX` and `EDX`: the `XCR0` bits this CPU supports, low
    /// and high. Zero where leaf 0DH does not exist.
    pub cpuid_d0_eax: u32,
    pub cpuid_d0_edx: u32,
    /// `CPUID.(0DH,1):EAX`: bit 0 is `XSAVEOPT`.
    pub cpuid_d1_eax: u32,
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
    /// The area the CPU reports for the declared `XCR0`, past [`IMAGE_BYTES`].
    AreaTooLarge(u32),
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoXsave => "it has no XSAVE (CPUID.01H:ECX[26])",
            Self::NoXsaveopt => "it has no XSAVEOPT (CPUID.(0DH,1):EAX[0]), the one save this kernel uses",
            Self::Missing(_) => "it does not support x87, SSE and AVX state in XCR0",
            Self::PartialAvx512(_) => {
                "it enumerates part of the AVX-512 state group, which XCR0 takes whole or not at all"
            }
            Self::AreaTooLarge(_) => "its save area is larger than the first-entry image",
        }
    }
}

/// The `XCR0` this CPU is declared with.
pub fn xcr0(facts: &Facts) -> Result<u64, Refusal> {
    if facts.cpuid_1_ecx & (1 << 26) == 0 {
        return Err(Refusal::NoXsave);
    }
    if facts.cpuid_d1_eax & 1 == 0 {
        return Err(Refusal::NoXsaveopt);
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

/// The bytes one save writes, from `CPUID.(0DH,0):EBX` read once the
/// declared `XCR0` is in place, and no more than [`IMAGE_BYTES`].
pub fn area_bytes(cpuid_d0_ebx: u32) -> Result<usize, Refusal> {
    match cpuid_d0_ebx as usize {
        bytes if bytes > IMAGE_BYTES => Err(Refusal::AreaTooLarge(cpuid_d0_ebx)),
        bytes => Ok(bytes),
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

    /// A CPU with AVX and no AVX-512: XCR0 bits x87, SSE, AVX and PKRU, as
    /// the AMD bench's Zen parts enumerate them.
    fn avx_only() -> Facts {
        Facts { cpuid_1_ecx: 1 << 26, cpuid_d0_eax: 0x207, cpuid_d0_edx: 0, cpuid_d1_eax: 1 }
    }

    #[test]
    fn the_t14_declares_avx512_and_nothing_past_it() {
        // Its CPUID offers PKRU (bit 9) as well; PKRU is not this declaration's.
        assert_eq!(t14_facts().cpuid_d0_eax & (1 << 9), 1 << 9);
        assert_eq!(xcr0(&t14_facts()), Ok(0xe7));
    }

    #[test]
    fn a_cpu_without_avx512_declares_avx() {
        assert_eq!(xcr0(&avx_only()), Ok(0x7));
    }

    #[test]
    fn a_cpu_without_xsave_is_refused() {
        let facts = Facts { cpuid_1_ecx: t14_facts().cpuid_1_ecx & !(1 << 26), ..t14_facts() };
        assert_eq!(xcr0(&facts), Err(Refusal::NoXsave));
    }

    #[test]
    fn a_cpu_without_xsaveopt_is_refused() {
        let facts = Facts { cpuid_d1_eax: 0, ..t14_facts() };
        assert_eq!(xcr0(&facts), Err(Refusal::NoXsaveopt));
    }

    #[test]
    fn a_cpu_without_avx_state_is_refused() {
        let facts = Facts { cpuid_d0_eax: 0x3, ..avx_only() };
        assert_eq!(xcr0(&facts), Err(Refusal::Missing(AVX)));
    }

    #[test]
    fn part_of_the_avx512_group_is_refused() {
        for partial in [OPMASK, ZMM_HI256, HI16_ZMM, OPMASK | HI16_ZMM, ZMM_HI256 | HI16_ZMM] {
            let facts = Facts { cpuid_d0_eax: 0x7 | partial as u32, ..avx_only() };
            assert_eq!(xcr0(&facts), Err(Refusal::PartialAvx512(partial)), "{partial:#x}");
        }
    }

    #[test]
    fn an_area_up_to_the_first_entry_image_is_taken_as_reported() {
        // SDM Vol. 1 §13.4: 512 legacy bytes and the 64-byte header, then
        // YMM_Hi128's 256 bytes at offset 576.
        assert_eq!(area_bytes(832), Ok(832));
        assert_eq!(area_bytes(IMAGE_BYTES as u32), Ok(IMAGE_BYTES));
    }

    #[test]
    fn an_area_past_the_first_entry_image_is_refused() {
        // The T14's own figure with PKRU in XCR0, which this kernel never declares.
        let with_pkru = t14(0xd, 0)[1];
        assert_eq!(with_pkru, IMAGE_BYTES as u32 + 8);
        assert_eq!(area_bytes(with_pkru), Err(Refusal::AreaTooLarge(with_pkru)));
    }
}
