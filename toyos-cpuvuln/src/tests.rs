//! Facts in, Linux's lines out, judged against `grep .` over the
//! vulnerabilities directory.
//!
//! Both fixtures are provisional: their facts are read off documents and their
//! lines are Linux's source read by hand, so they hold this crate to one reading
//! of that source and not yet to the oracle. S0 of
//! `issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`
//! captures both machines under the pinned Linux; its facts replace these and
//! its lines replace `fixtures/provisional/`.

extern crate std;

use std::string::ToString;
use std::vec::Vec;

use super::*;

const PREFIX: &str = "/sys/devices/system/cpu/vulnerabilities/";

/// The T14's i5-1135G7 as documented: `GenuineIntel`, family 6 model 0x8C
/// (TIGERLAKE_L) stepping 1, four cores of two threads, eIBRS, not affected by
/// Meltdown, L1TF, MDS, TAA or ITLB multihit, GDS microcode present and
/// unlocked. Only the bits `decide` reads are set, and the microcode revision,
/// which no `spectre_bad_microcodes` row for this model reads, is unknown here.
const T14: Facts = Facts {
    vendor: Vendor::from_id(b"GenuineIntel"),
    signature: 0x0008_06C1,
    microcode: 0,
    cpuid_1_ecx: CPUID_1_ECX_AVX | CPUID_1_ECX_RDRAND,
    cpuid_7_0_ebx: CPUID_7_0_EBX_RDSEED,
    cpuid_7_0_edx: CPUID_7_0_EDX_MD_CLEAR
        | CPUID_7_0_EDX_SPEC_CTRL
        | CPUID_7_0_EDX_INTEL_STIBP
        | CPUID_7_0_EDX_FLUSH_L1D
        | CPUID_7_0_EDX_ARCH_CAPABILITIES
        | CPUID_7_0_EDX_SPEC_CTRL_SSBD,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: 0,
    cpuid_8000_0021_eax: 0,
    arch_capabilities: ARCH_CAP_RDCL_NO
        | ARCH_CAP_IBRS_ALL
        | ARCH_CAP_MDS_NO
        | ARCH_CAP_PSCHANGE_MC_NO
        | ARCH_CAP_TAA_NO
        | ARCH_CAP_GDS_CTRL,
    mcu_opt_ctrl: Some(0),
    smt: true,
};

/// `-cpu qemu64,+rdrand,+smap,+fsgsbase,+x2apic,+smep` under TCG at the QEMU
/// `.github/qemu-version` pins, v11.1.1, read off its `target/i386/cpu.c`: the
/// `qemu64` model is `AuthenticAMD` family 15 model 107 stepping 1 with
/// `xlevel` 0x8000000A, so leaf 0x80000021 is out of reach (3545-3563); every
/// model gets the hypervisor bit (8486); system-mode TCG enumerates no
/// SPEC_CTRL, ARCH_CAPABILITIES, SSBD or 0x80000008:EBX speculation bit
/// (996-1001, 1024-1034); the microcode revision is its AMD default
/// (10187-10197). `-smp 2` gives one thread per core. Only the bits `decide`
/// reads are set.
const TCG: Facts = Facts {
    vendor: Vendor::from_id(b"AuthenticAMD"),
    signature: 0x0006_0FB1,
    microcode: 0x0100_0065,
    cpuid_1_ecx: CPUID_1_ECX_RDRAND | CPUID_1_ECX_HYPERVISOR,
    cpuid_7_0_ebx: 0,
    cpuid_7_0_edx: 0,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: 0,
    cpuid_8000_0021_eax: 0,
    arch_capabilities: 0,
    mcu_opt_ctrl: None,
    smt: false,
};

/// `facts`' line for every file `capture` names, and `capture` names each
/// file once.
fn assert_capture(facts: &Facts, capture: &str) {
    let d = decide(facts).expect("decided");
    let mut named = Vec::new();
    for l in capture.lines() {
        let (file, want) = l
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.split_once(':'))
            .unwrap_or_else(|| panic!("{l:?} is not `grep .` over {PREFIX}"));
        let v = *Vuln::ALL
            .iter()
            .find(|v| v.name() == file)
            .unwrap_or_else(|| panic!("{file}: a file Linux does not have"));
        let got = d.line(v).map(|line| line.to_string());
        assert_eq!(got.as_deref(), Ok(want), "{file}");
        named.push(file);
    }
    named.sort_unstable();
    let mut all: Vec<_> = Vuln::ALL.iter().map(|v| v.name()).collect();
    all.sort_unstable();
    assert_eq!(named, all, "the capture names every file once");
}

#[test]
fn the_t14s_facts_give_its_lines() {
    assert_eq!(Ident::new(T14.vendor, T14.signature).model, 0x8C);
    assert_capture(&T14, include_str!("../fixtures/provisional/t14.txt"));
}

#[test]
fn the_tcg_models_facts_give_its_lines() {
    let id = Ident::new(TCG.vendor, TCG.signature);
    assert_eq!((id.family, id.model, id.stepping), (15, 107, 1));
    assert_capture(&TCG, include_str!("../fixtures/provisional/tcg.txt"));
}

/// The track's exit control: the T14 with `IA32_ARCH_CAPABILITIES` read as 0.
/// Without `IBRS_ALL` it is in retpoline mode, without `GDS_CTRL` it has no GDS
/// microcode, and without `RDCL_NO` and `PSCHANGE_MC_NO` Linux's L1TF and
/// ITLB multihit lines turn on state the facts do not carry.
#[test]
fn the_t14_with_arch_capabilities_read_as_zero() {
    let d = decide(&Facts { arch_capabilities: 0, mcu_opt_ctrl: None, ..T14 }).expect("decided");
    let line = |v| d.line(v).map(|l| l.to_string());
    assert_eq!(
        line(Vuln::SpectreV2).as_deref(),
        Ok(
            "Mitigation: Retpolines; IBPB: conditional; IBRS_FW; STIBP: conditional; RSB filling; \
             PBRSB-eIBRS: Not affected; BHI: Not affected"
        )
    );
    assert_eq!(line(Vuln::GatherDataSampling).as_deref(), Ok("Vulnerable: No microcode"));
    assert_eq!(line(Vuln::Meltdown).as_deref(), Ok("Mitigation: PTI"));
    assert_eq!(line(Vuln::Mds).as_deref(), Ok("Mitigation: Clear CPU buffers; SMT vulnerable"));
    assert_eq!(line(Vuln::L1tf).err(), Some(Unmodelled::L1tf));
    assert_eq!(line(Vuln::ItlbMultihit).err(), Some(Unmodelled::ItlbMultihit));
}

#[test]
fn amd_from_family_0x15_and_hygon_are_refused() {
    let zen3 = Facts { signature: 0x00A0_0F11, ..TCG };
    assert_eq!(decide(&zen3).err(), Some(Refused::AmdFamily(0x19)));
    let hygon = Facts { vendor: Vendor::from_id(b"HygonGenuine"), ..TCG };
    assert_eq!(decide(&hygon).err(), Some(Refused::Hygon));
}
