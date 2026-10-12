//! Whether AMD erratum 1386, "XSAVES Instruction May Fail to Save XMM
//! Registers to the Provided State Save Area", names a CPU: after an `XRSTORS`
//! that put the XMM registers in their initial state, an `XSAVES` may skip
//! them while `MXCSR` has changed, and a later restore hands a thread an older
//! XMM value of its own.
//!
//! **AMD's revision guides decide, not Linux**, wherever AMD publishes one,
//! each read at the revision named here, and each saying "No fix planned", so
//! no microcode revision lifts the erratum:
//!
//! | guide | parts | lists 1386 against |
//! |---|---|---|
//! | 55449 rev 1.21 | family 17h models 00h-0Fh | ZP-B2 (`00800F12h`) alone; not ZP-B1 (`00800F11h`) or PiR-B2 (`00800F82h`) |
//! | 56323 rev 1.01 | family 17h models 30h-3Fh | Rome-B0 (`00830F10h`), its one revision |
//! | 56683 rev 1.08 | family 19h models 00h-0Fh | Milan-B1 (`00A00F11h`) and Milan-B2 (`00A00F12h`) |
//! | 57095 rev 1.01 | family 19h models 10h-1Fh | nothing |
//!
//! AMD publishes no guide for family 17h's other models, so there the scope
//! is the models Linux files as Zen1 or Zen2 (`bsp_init_amd`,
//! `arch/x86/kernel/cpu/amd.c:482-492` at upstream `87db7795`, not at this
//! crate's pinned tag), each of which runs `fix_erratum_1386` (`:887-903`);
//! `issues/amd-erratum-1386s-scope-rests-on-linux-where-amd-publishes-no-revision-guide.md`
//! owns that gap.

use crate::{Ident, Vendor};

/// `(family, first model, last model)` the erratum names.
const NAMED: [(u32, u32, u32); 7] = [
    // 55449's and 56323's ranges, and Linux's Zen1 and Zen2 beside them.
    (0x17, 0x00, 0x2f),
    (0x17, 0x30, 0x4f),
    (0x17, 0x50, 0x5f),
    (0x17, 0x60, 0x7f),
    (0x17, 0x90, 0x91),
    (0x17, 0xa0, 0xaf),
    // 56683's.
    (0x19, 0x00, 0x0f),
];

/// `(family, model, stepping)` of the revisions 55449 lists without it.
const CLEARED: [(u32, u32, u32); 2] = [(0x17, 0x01, 0x1), (0x17, 0x08, 0x2)];

/// Whether the erratum names the CPU whose CPUID.0 vendor is `vendor` and
/// whose CPUID.1:EAX is `signature`, so that it must not save with `XSAVES`.
pub fn erratum_1386(vendor: Vendor, signature: u32) -> bool {
    let id = Ident::new(vendor, signature);
    id.vendor == Vendor::Amd
        && NAMED.iter().any(|&(family, first, last)| id.family == family && (first..=last).contains(&id.model))
        && !CLEARED.contains(&(id.family, id.model, id.stepping))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amd(signature: u32) -> bool {
        erratum_1386(Vendor::Amd, signature)
    }

    #[test]
    fn each_revision_amds_guides_list_it_against_is_named() {
        for signature in [0x0080_0f12, 0x0083_0f10, 0x00a0_0f11, 0x00a0_0f12] {
            assert!(amd(signature), "{signature:#x}");
        }
    }

    #[test]
    fn the_revisions_55449_lists_without_it_are_not_named() {
        assert!(!amd(0x0080_0f11));
        assert!(!amd(0x0080_0f82));
    }

    #[test]
    fn genoa_which_57095_lists_without_it_is_not_named() {
        assert!(!amd(0x00a1_0f11));
    }

    /// The AMD bench, a Ryzen 7 4800U: family 17h model 60h as its Linux
    /// reports it, at whichever stepping, which its recorded words do not
    /// carry.
    #[test]
    fn the_amd_bench_is_named_at_every_stepping() {
        for stepping in 0..16 {
            assert!(amd(0x0086_0f00 | stepping), "stepping {stepping}");
        }
    }

    #[test]
    fn a_family_17h_model_no_source_names_is_not_named() {
        for model in [0x80u32, 0x8f, 0x92, 0x9f, 0xb0, 0xff] {
            assert!(!amd(0x0080_0f00 | (model >> 4) << 16 | (model & 0xf) << 4), "{model:#x}");
        }
    }

    /// The T14's Tiger Lake, TCG's `qemu64` (`AuthenticAMD` family 0xF), and
    /// a Hygon family 18h part, Zen1's sibling, which no AMD guide covers.
    #[test]
    fn no_other_vendor_or_family_is_named() {
        assert!(!erratum_1386(Vendor::Intel, 0x0008_06c1));
        assert!(!erratum_1386(Vendor::Intel, 0x0080_0f12));
        assert!(!amd(0x0006_0fb1));
        assert!(!erratum_1386(Vendor::Hygon, 0x0090_0f02));
    }
}
