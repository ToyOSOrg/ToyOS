//! Whether a CPU's `INVLPG` drops a global translation in every PCID, which a
//! kernel whose kernel-half leaves are global relies on while `CR4.PCIDE` is
//! set: Intel's erratum "INVLPG May Invalidate Global TLB Entries Only For The
//! Current PCID" (ADL063, RPL043), on the CPUs [`table`](crate::table) names,
//! below the microcode revision that fixed it.
//!
//! **Intel's verdict, not Linux's**: the specification updates are the
//! source, and where they part from `invlpg_miss_ids` (`arch/x86/mm/init.c`)
//! they decide. RPL043 marks the 6P+0E die N/A, which Linux's model match
//! takes in; the erratum "does not apply in VMX non-root operation", which
//! Linux does not read; and an update revision is signed (SDM Vol. 3A Table
//! 12-7), where Linux compares unsigned.

use crate::{table, Ident, Vendor, CPUID_1_ECX_HYPERVISOR};

/// What one CPU says about itself, read on that CPU.
#[derive(Clone, Copy, Debug)]
pub struct InvlpgFacts {
    pub vendor: Vendor,
    /// CPUID.1:EAX.
    pub signature: u32,
    pub cpuid_1_ecx: u32,
    /// `IA32_BIOS_SIGN_ID[63:32]` after the read SDM Vol. 3A §12.11.7.1
    /// defines, present exactly where the vendor is Intel: the procedure is
    /// Intel's.
    pub microcode: Option<u32>,
}

/// Whether `INVLPG` may leave a global translation cached for another PCID: a
/// CPU Intel names, outside VMX non-root operation (CPUID.1:ECX[31]), running
/// microcode older than the fix.
///
/// Panics where the facts contradict their own documentation: a revision read
/// on a vendor that is not Intel, or none on one that is.
pub fn invlpg_misses_global(facts: &InvlpgFacts) -> bool {
    assert_eq!(
        facts.microcode.is_some(),
        facts.vendor == Vendor::Intel,
        "the microcode revision is read exactly where the vendor is Intel",
    );
    let guest = facts.cpuid_1_ecx & CPUID_1_ECX_HYPERVISOR != 0;
    table::invlpg_fixed_at(&Ident::new(facts.vendor, facts.signature))
        .is_some_and(|fixed| !guest && (facts.microcode.expect("asserted above") as i32) < fixed)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    /// A native Intel CPU of `signature` running `microcode`.
    fn native(signature: u32, microcode: u32) -> InvlpgFacts {
        InvlpgFacts { vendor: Vendor::Intel, signature, cpuid_1_ecx: 0, microcode: Some(microcode) }
    }

    /// Every signature the two specification updates name, from each one's
    /// Table 2-1 beside its erratum's summary row, and Alder Lake-N's from its
    /// own Table 2-1, each with Intel's fixed revision for its model.
    const NAMED: [(u32, u32, &str); 9] = [
        // 682436-040: ADL063 "Fixed" on S, H/P, U and HX.
        (0x0009_0672, 0x2e, "ADL-S 8+8, ADL-HX 8+8"),
        (0x0009_0675, 0x2e, "ADL-S 6+0"),
        (0x0009_06a3, 0x42c, "ADL-H 6+8, ADL-P 6+8"),
        (0x0009_06a4, 0x42c, "ADL-U15W 2+8, ADL-U9W 2+8"),
        // 740518-031: RPL043 "Fixed" on every column but S 6+0.
        (0x000b_0671, 0x118, "RPL-S 8P+16E, RPL-HX 8P+16E, RPL-E 8P+0E"),
        (0x000b_06f2, 0x2e, "RPL-S 8P+8E, RPL-HX 8P+8E"),
        (0x000b_06a2, 0x4117, "RPL-P 6P+8E, RPL-H 6P+8E"),
        (0x000b_06a3, 0x4117, "RPL-U 2P+8E"),
        // 764616-004 lists the N-series by this signature alone.
        (0x000b_06e0, 0x11, "Alder Lake-N"),
    ];

    #[test]
    fn every_cpu_intel_names_withholds_pcid_below_its_fix_and_only_there() {
        for (signature, fixed, name) in NAMED {
            assert!(invlpg_misses_global(&native(signature, fixed - 1)), "{name} one revision below its fix");
            assert!(!invlpg_misses_global(&native(signature, fixed)), "{name} at its fix");
        }
    }

    /// RPL043's summary row reads N/A under S 6+0, signature `0xB06F5`.
    #[test]
    fn the_die_rpl043_marks_not_applicable_keeps_pcid() {
        assert!(!invlpg_misses_global(&native(0x000b_06f5, 0)));
    }

    /// "This erratum does not apply to a guest operating system running in VMX
    /// non-root operation" (ADL063, RPL043).
    #[test]
    fn a_guest_keeps_pcid() {
        let guest = InvlpgFacts { cpuid_1_ecx: CPUID_1_ECX_HYPERVISOR, ..native(0x0009_06a4, 0) };
        assert!(!invlpg_misses_global(&guest));
    }

    /// Table 12-7: an update revision is a signed 32-bit number.
    #[test]
    fn a_revision_with_its_sign_bit_set_is_older_than_the_fix() {
        assert!(invlpg_misses_global(&native(0x000b_06a2, 0x8000_4117)));
    }

    /// The T14's CPU 0 off `fixtures/t14/`: its signature from `cpuid.txt`, its
    /// revision from `msr.txt`'s 0x8B, as Linux read both. Tiger Lake is in
    /// neither table.
    #[test]
    fn the_t14_keeps_pcid() {
        let words = |text: &'static str, first: &str| -> Vec<u64> {
            text.lines()
                .map(|l| l.split(' ').map(|w| u64::from_str_radix(w, 16).expect(w)).collect::<Vec<_>>())
                .find(|l| l[..2] == [u64::from_str_radix(first, 16).unwrap(), 0])
                .expect("a line the capture read")
        };
        let leaf_1 = words(include_str!("../fixtures/t14/cpuid.txt"), "1");
        let bios_sign_id = words(include_str!("../fixtures/t14/msr.txt"), "8b")[2];
        let t14 = InvlpgFacts {
            vendor: Vendor::Intel,
            signature: leaf_1[2] as u32,
            cpuid_1_ecx: leaf_1[4] as u32,
            microcode: Some((bios_sign_id >> 32) as u32),
        };
        assert_eq!((t14.signature, t14.microcode), (0x0008_06c1, Some(0xbe)));
        assert!(!invlpg_misses_global(&t14));
    }

    #[test]
    fn another_vendor_reads_no_revision_and_keeps_pcid() {
        let amd = InvlpgFacts { vendor: Vendor::Amd, microcode: None, ..native(0x0009_06a4, 0) };
        assert!(!invlpg_misses_global(&amd));
    }

    #[test]
    #[should_panic(expected = "read exactly where the vendor is Intel")]
    fn an_intel_cpu_without_its_revision_is_a_contradiction() {
        invlpg_misses_global(&InvlpgFacts { microcode: None, ..native(0x0008_06c1, 0) });
    }
}
