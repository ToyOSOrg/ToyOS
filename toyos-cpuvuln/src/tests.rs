//! Facts in, Linux's lines and selection out, the lines judged against
//! `grep .` over the vulnerabilities directory.

extern crate std;

use std::string::ToString;
use std::vec::Vec;

use super::*;

const PREFIX: &str = "/sys/devices/system/cpu/vulnerabilities/";

/// The T14's i5-1135G7 as S0 read it under the pinned Linux: PR #601's
/// `toyos-t14linux/s0/t14/cpuid.txt`, `msr.txt` and `cpuinfo.txt` at commit
/// 44eb3c2e. Every MSR reads the same on all eight CPUs.
const T14: Facts = Facts {
    vendor: Vendor::from_id(b"GenuineIntel"),
    signature: 0x0008_06c1,
    microcode: 0xbe,
    cpuid_1_ecx: 0x7ffa_fbbf,
    cpuid_7_0_ebx: 0xf3bf_a7eb,
    cpuid_7_0_edx: 0xfc10_0710,
    cpuid_7_2_edx: 0x0000_0001,
    cpuid_8000_0008_ebx: 0,
    // CPUID.0x80000000:EAX is 0x80000008.
    cpuid_8000_0021_eax: 0,
    cpuid_8000_0021_ecx: 0,
    arch_capabilities: 0x0a00_5c6b,
    mcu_opt_ctrl: Some(0),
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    // `siblings 8`, `cpu cores 4`.
    smt: true,
};

/// The TCG model: `qemu64`, `AuthenticAMD` family 0xF model 0x6B stepping 1,
/// the signature S0's capture verifies; the bits `decide` reads in leaves 7
/// and 0x80000008 are clear in that model, and it has no leaf 0x80000021.
const TCG: Facts = Facts {
    vendor: Vendor::from_id(b"AuthenticAMD"),
    signature: 0x0006_0fb1,
    microcode: 0,
    cpuid_1_ecx: 0,
    cpuid_7_0_ebx: 0,
    cpuid_7_0_edx: 0,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: 0,
    cpuid_8000_0021_eax: 0,
    cpuid_8000_0021_ecx: 0,
    arch_capabilities: 0,
    mcu_opt_ctrl: None,
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    smt: false,
};

/// An EPYC 7713 (Milan, family 0x19 model 1 stepping 1, the family and model
/// of the nightly's EPYC 7763) as a KVM guest: InstLatx64's bare-metal dump
/// `AuthenticAMD/AuthenticAMD0A00F11_K19_Milan_CPUID1.txt` (commit 2dc186e9)
/// with CPUID.1:ECX[31] set, `-smp cores=N`, and no microcode revision, which
/// no line reads under a hypervisor. It does not enumerate ARCH_CAPABILITIES.
const MILAN_GUEST: Facts = Facts {
    vendor: Vendor::from_id(b"AuthenticAMD"),
    signature: 0x00a0_0f11,
    microcode: 0,
    cpuid_1_ecx: 0x7eda_320b | CPUID_1_ECX_HYPERVISOR,
    cpuid_7_0_ebx: 0x219c_97a9,
    cpuid_7_0_edx: 0x0000_0010,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: 0x91be_f75f,
    cpuid_8000_0021_eax: 0x0000_204d,
    cpuid_8000_0021_ecx: 0,
    arch_capabilities: 0,
    mcu_opt_ctrl: None,
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    smt: false,
};

/// An EPYC 9655 (Turin, family 0x1A model 2 stepping 1, the family of the
/// nightly's EPYC 9V45) as a KVM guest: InstLatx64's bare-metal dump
/// `AuthenticAMD/AuthenticAMD0B00F21_K20_Turin_01_CPUID.txt` (commit b499237d),
/// shaped as [`MILAN_GUEST`] is.
const TURIN_GUEST: Facts = Facts {
    vendor: Vendor::from_id(b"AuthenticAMD"),
    signature: 0x00b0_0f21,
    microcode: 0,
    cpuid_1_ecx: 0x7efa_320b | CPUID_1_ECX_HYPERVISOR,
    cpuid_7_0_ebx: 0xf1bf_97ab,
    cpuid_7_0_edx: 0x1000_0110,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: 0x79be_f25f,
    cpuid_8000_0021_eax: 0xd93f_ffcf,
    cpuid_8000_0021_ecx: 0,
    arch_capabilities: 0,
    mcu_opt_ctrl: None,
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    smt: false,
};

/// [`Decision`]'s selection, every public field: the destructuring in
/// [`state`] names each, so a field added there fails to build here.
#[derive(Debug, PartialEq)]
struct State {
    tsx: Tsx,
    spectre_v2: SpectreV2,
    bhi: Bhi,
    ibpb: Ibpb,
    ibrs_fw: bool,
    stibp: Stibp,
    retbleed: Retbleed,
    ssb: Ssb,
    mds: Mds,
    taa: Taa,
    mmio: Mmio,
    rfds: Rfds,
    srbds: Option<Srbds>,
    gds: Option<Gds>,
    its: Its,
    vmscape: Vmscape,
    srso: Option<Srso>,
    tsa: Option<Tsa>,
    sbpb: bool,
    clear_cpu_buf: bool,
    efer_autoibrs: bool,
    spec_ctrl: u64,
}

fn state(d: &Decision) -> State {
    let Decision {
        tsx,
        spectre_v2,
        bhi,
        ibpb,
        ibrs_fw,
        stibp,
        retbleed,
        ssb,
        mds,
        taa,
        mmio,
        rfds,
        srbds,
        gds,
        its,
        vmscape,
        srso,
        tsa,
        sbpb,
        clear_cpu_buf,
        efer_autoibrs,
        spec_ctrl,
        bugs: _,
        hypervisor: _,
        autoibrs: _,
        smt: _,
    } = *d;
    State {
        tsx,
        spectre_v2,
        bhi,
        ibpb,
        ibrs_fw,
        stibp,
        retbleed,
        ssb,
        mds,
        taa,
        mmio,
        rfds,
        srbds,
        gds,
        its,
        vmscape,
        srso,
        tsa,
        sbpb,
        clear_cpu_buf,
        efer_autoibrs,
        spec_ctrl,
    }
}

/// Nothing selected: a CPU no vulnerability reaches.
const NONE: State = State {
    tsx: Tsx::NotSupported,
    spectre_v2: SpectreV2::None,
    bhi: Bhi::NotAffected,
    ibpb: Ibpb::Absent,
    ibrs_fw: false,
    stibp: Stibp::None,
    retbleed: Retbleed::None,
    ssb: Ssb::None,
    mds: Mds::Off,
    taa: Taa::Off,
    mmio: Mmio::Off,
    rfds: Rfds::Off,
    srbds: None,
    gds: None,
    its: Its::Off,
    vmscape: Vmscape::None,
    srso: None,
    tsa: None,
    sbpb: false,
    clear_cpu_buf: false,
    efer_autoibrs: false,
    spec_ctrl: 0,
};

/// `facts`' line for every file `capture` names, and `capture` names each
/// file once.
fn assert_capture(facts: &Facts, capture: &str) -> Decision {
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
    d
}

fn line(facts: &Facts, v: Vuln) -> std::string::String {
    decide(facts).expect("decided").line(v).expect("modelled").to_string()
}

#[test]
fn the_t14s_facts_give_its_lines_and_its_spec_ctrl() {
    assert_eq!(Ident::new(T14.vendor, T14.signature).model, 0x8C);
    let d = assert_capture(&T14, include_str!("../fixtures/t14.txt"));
    // `spec_ctrl` against S0's `msr.txt`: 0x48 reads 0x1 on every CPU.
    assert_eq!(
        state(&d),
        State {
            spectre_v2: SpectreV2::Eibrs,
            bhi: Bhi::SwLoop,
            ibpb: Ibpb::Conditional,
            ssb: Ssb::Prctl,
            gds: Some(Gds::Full),
            its: Its::AlignedThunks,
            spec_ctrl: 0x1,
            ..NONE
        }
    );
}

/// S0 verifies the TCG model's signature and nothing else it could read, so
/// its lines are held over either value of every other fact they could rest on.
#[test]
fn the_tcg_models_signature_gives_its_lines() {
    let id = Ident::new(TCG.vendor, TCG.signature);
    assert_eq!((id.family, id.model, id.stepping), (15, 107, 1));
    for hypervisor in [0, CPUID_1_ECX_HYPERVISOR] {
        for rdrand in [0, CPUID_1_ECX_RDRAND] {
            for microcode in [0, u32::MAX] {
                for smt in [false, true] {
                    let facts =
                        Facts { cpuid_1_ecx: hypervisor | rdrand, microcode, smt, ..TCG };
                    let d = assert_capture(&facts, include_str!("../fixtures/tcg.txt"));
                    assert_eq!(state(&d), State { spectre_v2: SpectreV2::Retpoline, ..NONE });
                }
            }
        }
    }
}

/// The track's exit control: the T14 with `IA32_ARCH_CAPABILITIES` read as 0.
/// Without `IBRS_ALL` it is in retpoline mode, without `GDS_CTRL` it has no GDS
/// microcode, without `MDS_NO` it clears CPU buffers, and without `RDCL_NO` and
/// `PSCHANGE_MC_NO` Linux's L1TF and ITLB multihit lines turn on state the
/// facts do not carry.
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
    assert_eq!(
        state(&d),
        State {
            spectre_v2: SpectreV2::Retpoline,
            ibpb: Ibpb::Conditional,
            ibrs_fw: true,
            stibp: Stibp::Prctl,
            ssb: Ssb::Prctl,
            mds: Mds::Full,
            gds: Some(Gds::UcodeNeeded),
            its: Its::AlignedThunks,
            clear_cpu_buf: true,
            ..NONE
        }
    );
}

/// `x86_match_cpu` matches a row's steppings as a mask: KABYLAKE_L's first row
/// covers steppings 0 to 0xB and leaves ITS out.
#[test]
fn a_blacklist_row_matches_only_its_steppings() {
    let at = |stepping: u32| {
        line(&Facts { signature: 0x0008_06e0 | stepping, ..T14 }, Vuln::IndirectTargetSelection)
    };
    assert_eq!(at(0xB), "Not affected");
    assert_eq!(at(0xC), "Mitigation: Aligned branch/return thunks");
}

/// The first matching row decides: SKYLAKE_X's steppings 0 to 5 row, without
/// ITS, stands before its any-stepping row, with it.
#[test]
fn the_first_matching_blacklist_row_decides() {
    let at = |stepping: u32| {
        line(&Facts { signature: 0x0005_0650 | stepping, ..T14 }, Vuln::IndirectTargetSelection)
    };
    assert_eq!(at(5), "Not affected");
    assert_eq!(at(6), "Mitigation: Aligned branch/return thunks");
}

/// KABYLAKE stepping 0xA's bad microcode is 0x80 and below: there Linux drops
/// IBRS, IBPB, STIBP and SSBD.
#[test]
fn spectre_bad_microcode_includes_its_bound() {
    let kabylake = |microcode| {
        decide(&Facts {
            signature: 0x0009_06ea,
            microcode,
            arch_capabilities: 0,
            mcu_opt_ctrl: None,
            ..T14
        })
        .expect("decided")
    };
    let bad = kabylake(0x80);
    assert_eq!((bad.ibpb, bad.ssb, bad.spectre_v2), (Ibpb::Absent, Ssb::None, SpectreV2::Retpoline));
    let good = kabylake(0x81);
    assert_eq!((good.ibpb, good.ssb, good.spectre_v2), (Ibpb::Conditional, Ssb::Prctl, SpectreV2::Ibrs));
}

#[test]
fn a_guest_cannot_know_its_gds_mitigation() {
    let guest = Facts { cpuid_1_ecx: T14.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR, ..T14 };
    assert_eq!(line(&guest, Vuln::GatherDataSampling), "Unknown: Dependent on hypervisor status");
}

// The two fixtures below are this crate's reading of the pinned Linux, awaiting
// a capture on a nightly runner:
// `issues/build/no-nightly-runner-has-had-its-cpuid-and-vulnerability-lines-captured.md`.

#[test]
fn a_milan_kvm_guest_gives_the_tags_lines() {
    let d = assert_capture(&MILAN_GUEST, include_str!("../fixtures/awaiting-capture/milan-kvm.txt"));
    assert_eq!(
        state(&d),
        State {
            spectre_v2: SpectreV2::Retpoline,
            ibpb: Ibpb::Conditional,
            ibrs_fw: true,
            ssb: Ssb::Prctl,
            srso: Some(Srso::SafeRetUcodeNeeded),
            tsa: Some(Tsa::UcodeNeeded),
            clear_cpu_buf: true,
            ..NONE
        }
    );
}

#[test]
fn a_turin_kvm_guest_gives_the_tags_lines() {
    let d = assert_capture(&TURIN_GUEST, include_str!("../fixtures/awaiting-capture/turin-kvm.txt"));
    assert_eq!(
        state(&d),
        State {
            spectre_v2: SpectreV2::Eibrs,
            ibpb: Ibpb::Conditional,
            ssb: Ssb::Prctl,
            sbpb: true,
            efer_autoibrs: true,
            ..NONE
        }
    );
}

/// A Rome (Zen2, family 0x17 model 0x31) outside a hypervisor: its IBPB
/// implies `IBPB_BRTYPE`, so without SMT it is SRSO-immune, and with SMT the
/// untrained return thunk forces STIBP always on.
#[test]
fn zen2_selects_by_smt() {
    let rome = |smt| Facts {
        signature: 0x0083_0f10,
        cpuid_1_ecx: 0,
        cpuid_8000_0021_eax: 0,
        smt,
        ..MILAN_GUEST
    };
    let d = decide(&rome(false)).expect("decided");
    assert_eq!(
        d.line(Vuln::SpecRstackOverflow).expect("modelled").to_string(),
        "Mitigation: SMT disabled"
    );
    assert_eq!(
        d.line(Vuln::Retbleed).expect("modelled").to_string(),
        "Mitigation: untrained return thunk; SMT disabled"
    );
    let d = decide(&rome(true)).expect("decided");
    assert_eq!(
        d.line(Vuln::Retbleed).expect("modelled").to_string(),
        "Mitigation: untrained return thunk; SMT enabled with STIBP protection"
    );
    assert_eq!(
        state(&d),
        State {
            spectre_v2: SpectreV2::Retpoline,
            ibpb: Ibpb::Conditional,
            stibp: Stibp::StrictPreferred,
            retbleed: Retbleed::Unret,
            ssb: Ssb::Prctl,
            vmscape: Vmscape::IbpbExitToUser,
            srso: Some(Srso::SafeRet),
            spec_ctrl: SPEC_CTRL_STIBP,
            ..NONE
        }
    );
}

/// A Milan outside a hypervisor: its `PRED_CMD.SBPB` probe decides SRSO's
/// microcode, and `amd_check_tsa_microcode`'s row for 0xA0011 decides TSA's.
#[test]
fn zen3_native_reads_its_probe_and_its_tsa_microcode() {
    let milan = |microcode, sbpb| Facts {
        microcode,
        cpuid_1_ecx: MILAN_GUEST.cpuid_1_ecx & !CPUID_1_ECX_HYPERVISOR,
        sbpb_write_accepted: Some(sbpb),
        ..MILAN_GUEST
    };
    let native = State {
        spectre_v2: SpectreV2::Retpoline,
        ibpb: Ibpb::Conditional,
        ibrs_fw: true,
        ssb: Ssb::Prctl,
        vmscape: Vmscape::IbpbExitToUser,
        ..NONE
    };
    let d = decide(&milan(0x0a00_11d6, false)).expect("decided");
    assert_eq!(
        state(&d),
        State {
            srso: Some(Srso::SafeRetUcodeNeeded),
            tsa: Some(Tsa::UcodeNeeded),
            ..native
        }
    );
    let d = decide(&milan(0x0a00_11d7, true)).expect("decided");
    assert_eq!(
        state(&d),
        State { srso: Some(Srso::SafeRet), tsa: Some(Tsa::Full), clear_cpu_buf: true, ..native }
    );
    assert_eq!(d.line(Vuln::Tsa).expect("modelled").to_string(), "Mitigation: Clear CPU buffers");
}

#[test]
#[should_panic(expected = "PRED_CMD.SBPB is probed exactly where")]
fn a_native_zen3_without_its_probe_is_a_contradiction() {
    let _ = decide(&Facts { cpuid_1_ecx: 0, ..MILAN_GUEST });
}

#[test]
fn amd_outside_0x17_0x19_0x1a_from_0x15_and_hygon_are_refused() {
    for (signature, family) in [(0x0060_0f00, 0x15), (0x0070_0f00, 0x16), (0x00c0_0f00, 0x1B)] {
        assert_eq!(
            decide(&Facts { signature, ..TCG }).err(),
            Some(Refused::AmdFamily(family))
        );
    }
    let hygon = Facts { vendor: Vendor::from_id(b"HygonGenuine"), ..TCG };
    assert_eq!(decide(&hygon).err(), Some(Refused::Hygon));
}
