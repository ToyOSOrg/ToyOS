//! Which of a CPU's model-specific counters exist, as Linux's `msr` PMU decides
//! (`arch/x86/events/msr.c`): a counter a CPU lacks is `#GP` to read, and the
//! kernel has no fault fixup, so it reads only what this admits.
//!
//! **One departure from Linux**: `MSR_SMI_COUNT` is not admitted under a
//! hypervisor (`CPUID.1:ECX[31]`). Linux admits it there and its probe read
//! decides (`probe.c:43-47`), but a hypervisor answers it with its own count
//! for the guest (QEMU's `env->msr_smi_count`), not the firmware's. `APERF`
//! and `MPERF` are admitted wherever leaf 6 says, a hypervisor's included, as
//! Linux admits them: one that enumerates them and faults the read ends the
//! boot that read them first.

use crate::{table, Ident, Vendor, CPUID_1_ECX_HYPERVISOR};

/// What one CPU says about itself, read on that CPU.
#[derive(Clone, Copy, Debug)]
pub struct CounterFacts {
    pub vendor: Vendor,
    /// CPUID.1:EAX.
    pub signature: u32,
    pub cpuid_1_ecx: u32,
    /// 0 unless CPUID.0:EAX reaches 6, as `init_scattered_cpuid_features`
    /// reads it (`scattered.c:65-71`).
    pub cpuid_6_ecx: u32,
}

/// The counters a CPU has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters {
    /// `IA32_APERF` and `IA32_MPERF`.
    pub aperf_mperf: bool,
    /// `MSR_SMI_COUNT`.
    pub smi: bool,
}

/// `X86_FEATURE_APERFMPERF` (`scattered.c:27`).
const CPUID_6_ECX_APERFMPERF: u32 = 1 << 0;

pub fn counters(facts: &CounterFacts) -> Counters {
    Counters {
        // `test_aperfmperf` (`msr.c:20-23,158-159`), whatever the vendor.
        aperf_mperf: facts.cpuid_6_ecx & CPUID_6_ECX_APERFMPERF != 0,
        // `test_intel` (`msr.c:161`), outside a hypervisor.
        smi: table::counts_smis(&Ident::new(facts.vendor, facts.signature))
            && facts.cpuid_1_ecx & CPUID_1_ECX_HYPERVISOR == 0,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::collections::BTreeSet;
    use std::vec::Vec;

    use super::*;

    /// The T14's CPU 0, off `fixtures/t14/cpuid.txt`.
    fn t14() -> CounterFacts {
        let leaves: Vec<Vec<u32>> = include_str!("../fixtures/t14/cpuid.txt")
            .lines()
            .map(|l| l.split(' ').map(|w| u32::from_str_radix(w, 16).expect(w)).collect())
            .collect();
        let leaf = |n: u32| leaves.iter().find(|l| l[..2] == [n, 0]).expect("a leaf the capture read");
        let id: Vec<u8> = [leaf(0)[3], leaf(0)[5], leaf(0)[4]].iter().flat_map(|r| r.to_le_bytes()).collect();
        assert!(leaf(0)[2] >= 6, "the T14 reaches leaf 6");
        CounterFacts {
            vendor: Vendor::from_id(id[..].try_into().unwrap()),
            signature: leaf(1)[2],
            cpuid_1_ecx: leaf(1)[4],
            cpuid_6_ecx: leaf(6)[4],
        }
    }

    /// QEMU 11.1.1's `qemu64`, the TCG model: `AuthenticAMD` family 0xF model
    /// 0x6B (`target/i386/cpu.c:3545-3550`), and leaf 6's ECX zero on every
    /// model (`cpu.c:8777-8782`).
    const TCG: CounterFacts = CounterFacts {
        vendor: Vendor::Amd,
        signature: 0x0006_0fb1,
        cpuid_1_ecx: 0,
        cpuid_6_ecx: 0,
    };

    /// Linux on the T14 listed exactly the events whose test and probe read
    /// passed (`probe.c:28-59`), so its `perf stat` over them is its verdict on
    /// that CPU, and this is held to it.
    #[test]
    fn the_t14_has_what_linux_counted_on_it() {
        let counted: BTreeSet<&str> = include_str!("../fixtures/t14/perf-msr.txt")
            .lines()
            .filter_map(|l| l.split_whitespace().nth(2)?.strip_prefix("msr/")?.strip_suffix('/'))
            .collect();
        assert_eq!(counted, BTreeSet::from(["aperf", "mperf", "smi", "tsc"]));
        assert_eq!(counters(&t14()), Counters { aperf_mperf: true, smi: true });
    }

    #[test]
    fn the_tcg_model_has_none() {
        assert_eq!(counters(&TCG), Counters { aperf_mperf: false, smi: false });
    }

    /// Under a hypervisor the SMI count is the hypervisor's, and APERF and
    /// MPERF stay whatever leaf 6 says.
    #[test]
    fn a_guest_counts_no_smis() {
        let guest = CounterFacts { cpuid_1_ecx: t14().cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR, ..t14() };
        assert_eq!(counters(&guest), Counters { aperf_mperf: true, smi: false });
    }

    #[test]
    fn an_amd_with_leaf_6_has_aperf_and_mperf_and_no_smi_count() {
        let zen = CounterFacts { cpuid_6_ecx: CPUID_6_ECX_APERFMPERF, ..TCG };
        assert_eq!(counters(&zen), Counters { aperf_mperf: true, smi: false });
    }

    /// The model list is the T14's model and Linux's other rows, and nothing
    /// in family 6 that Linux leaves out.
    #[test]
    fn an_intel_model_off_the_list_counts_no_smis() {
        // Family 6 model 0x8C stepping 1 is the T14's; 0x66 is CANNONLAKE_L,
        // which `test_intel` does not name.
        let model = |m: u32| CounterFacts { signature: 0x0000_0601 | (m >> 4) << 16 | (m & 0xf) << 4, ..t14() };
        assert!(counters(&model(0x8C)).smi);
        assert!(!counters(&model(0x66)).smi);
        // Family 0xF Intel is outside `test_intel`'s family, and the T14's
        // family and model on another vendor outside its vendor.
        assert!(!counters(&CounterFacts { signature: 0x0000_0f41, ..t14() }).smi);
        assert!(!counters(&CounterFacts { vendor: Vendor::Centaur, ..t14() }).smi);
    }
}
