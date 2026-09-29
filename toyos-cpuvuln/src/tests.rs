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
/// no line reads under a hypervisor.
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

/// [`MILAN_GUEST`] outside a hypervisor, its `PRED_CMD.SBPB` probe answering
/// `sbpb`.
const fn native_milan(microcode: u32, sbpb: bool) -> Facts {
    Facts {
        microcode,
        cpuid_1_ecx: MILAN_GUEST.cpuid_1_ecx & !CPUID_1_ECX_HYPERVISOR,
        sbpb_write_accepted: Some(sbpb),
        ..MILAN_GUEST
    }
}

/// What [`native_milan`] selects whatever its microcode and probe.
const NATIVE_MILAN: State = State {
    spectre_v2: SpectreV2::Retpoline,
    ibpb: Ibpb::Conditional,
    ibrs_fw: true,
    ssb: Ssb::Prctl,
    vmscape: Vmscape::IbpbExitToUser,
    ..NONE
};

/// A Milan outside a hypervisor: its `PRED_CMD.SBPB` probe decides SRSO's
/// microcode, and `amd_check_tsa_microcode`'s row for 0xA0011 decides TSA's.
#[test]
fn zen3_native_reads_its_probe_and_its_tsa_microcode() {
    let d = decide(&native_milan(0x0a00_11d6, false)).expect("decided");
    assert_eq!(
        state(&d),
        State {
            srso: Some(Srso::SafeRetUcodeNeeded),
            tsa: Some(Tsa::UcodeNeeded),
            ..NATIVE_MILAN
        }
    );
    let d = decide(&native_milan(0x0a00_11d7, true)).expect("decided");
    assert_eq!(
        state(&d),
        State { srso: Some(Srso::SafeRet), tsa: Some(Tsa::Full), clear_cpu_buf: true, ..NATIVE_MILAN }
    );
    assert_eq!(d.line(Vuln::Tsa).expect("modelled").to_string(), "Mitigation: Clear CPU buffers");
}

/// A Genoa, family 0x19 model 0x11 stepping 1, which `bsp_init_amd` names Zen4
/// (`amd.c:625-627`): `tsa_init` sets `VERW_CLEAR` (`amd.c:522-525`) from row
/// 0xa1011's microcode 0x0a10114c up (`amd.c:491,514`), and without it TSA's
/// microcode is missing (`bugs.c:2928-2929`; `tsa_strings`, 2889 and 2892).
#[test]
fn zen4_reads_its_row_of_the_tsa_microcode_table() {
    let genoa = |microcode| Facts { signature: 0x00a1_0f11, ..native_milan(microcode, true) };
    let d = decide(&genoa(0x0a10_114c)).expect("decided");
    assert_eq!((d.tsa, d.clear_cpu_buf), (Some(Tsa::Full), true));
    assert_eq!(line(&genoa(0x0a10_114c), Vuln::Tsa), "Mitigation: Clear CPU buffers");
    let d = decide(&genoa(0x0a10_114b)).expect("decided");
    assert_eq!((d.tsa, d.clear_cpu_buf), (Some(Tsa::UcodeNeeded), false));
    assert_eq!(
        line(&genoa(0x0a10_114b), Vuln::Tsa),
        "Vulnerable: Clear CPU buffers attempted, no microcode"
    );
}

/// An AMD CPU is TSA-affected unless it reports both `TSA_SQ_NO` and
/// `TSA_L1_NO` (`common.c:1555-1561`), CPUID.0x80000021:ECX bits 1 and 2
/// (`scattered.c:52-53`). A guest's TSA microcode is missing
/// (`bugs.c:2928-2929`, `tsa_strings` 2889).
#[test]
fn tsa_is_ruled_out_only_by_both_its_no_bits() {
    let at = |ecx| line(&Facts { cpuid_8000_0021_ecx: ecx, ..MILAN_GUEST }, Vuln::Tsa);
    let affected = "Vulnerable: Clear CPU buffers attempted, no microcode";
    assert_eq!(at(CPUID_8000_0021_ECX_TSA_SQ_NO), affected);
    assert_eq!(at(CPUID_8000_0021_ECX_TSA_L1_NO), affected);
    assert_eq!(at(CPUID_8000_0021_ECX_TSA_SQ_NO | CPUID_8000_0021_ECX_TSA_L1_NO), "Not affected");
}

/// A native Milan with SMT: no retbleed row names family 0x19, so STIBP
/// always-on comes from `AMD_STIBP_ALWAYS_ON` alone (`bugs.c:1575-1577`);
/// `update_stibp_strict` sets `SPEC_CTRL.STIBP` for it (`bugs.c:2054-2068`,
/// 2976-2977) and `stibp_state` prints it (`bugs.c:3185-3186`).
#[test]
fn amd_stibp_always_on_is_strict_without_the_untrained_return_thunk() {
    assert_ne!(MILAN_GUEST.cpuid_8000_0008_ebx & CPUID_8000_0008_EBX_AMD_STIBP_ALWAYS_ON, 0);
    let milan = Facts { smt: true, ..native_milan(0x0a00_11d7, true) };
    let d = decide(&milan).expect("decided");
    assert_eq!(
        state(&d),
        State {
            stibp: Stibp::StrictPreferred,
            srso: Some(Srso::SafeRet),
            tsa: Some(Tsa::Full),
            clear_cpu_buf: true,
            spec_ctrl: SPEC_CTRL_STIBP,
            ..NATIVE_MILAN
        }
    );
    assert_eq!(
        line(&milan, Vuln::SpectreV2),
        "Mitigation: Retpolines; IBPB: conditional; IBRS_FW; STIBP: always-on; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: Not affected"
    );
}

/// AMD families 0x15 to 0x17 enumerating neither `AMD_SSBD` nor `VIRT_SSBD`
/// have SSBD exactly where `MSR_AMD64_LS_CFG` reads (`amd.c:576-596`), and SSB's
/// prctl mode needs SSBD (`bugs.c:2177-2178,2200-2202`; `ssb_strings`, 2123 and
/// 2125). Family 0x17 model 1 is Zen1 (`amd.c:602-607`).
#[test]
fn amd_0x15_to_0x17_take_ssbd_from_the_ls_cfg_probe() {
    for signature in [0x0060_0f00, 0x0070_0f00, 0x0080_0f11] {
        let at = |readable| Facts {
            signature,
            cpuid_1_ecx: 0,
            cpuid_8000_0008_ebx: MILAN_GUEST.cpuid_8000_0008_ebx
                & !(CPUID_8000_0008_EBX_AMD_SSBD | CPUID_8000_0008_EBX_VIRT_SSBD),
            cpuid_8000_0021_eax: 0,
            ls_cfg_readable: Some(readable),
            ..MILAN_GUEST
        };
        let d = decide(&at(true)).expect("decided");
        assert_eq!(d.ssb, Ssb::Prctl, "{signature:#x}");
        assert_eq!(
            line(&at(true), Vuln::SpecStoreBypass),
            "Mitigation: Speculative Store Bypass disabled via prctl"
        );
        let d = decide(&at(false)).expect("decided");
        assert_eq!(d.ssb, Ssb::None, "{signature:#x}");
        assert_eq!(line(&at(false), Vuln::SpecStoreBypass), "Vulnerable");
    }
}

/// A native family 0x19 reporting `SRSO_USER_KERNEL_NO` leaves safe RET for
/// the VM-exit arm (`bugs.c:2715-2716`): `SRSO_BP_SPEC_REDUCE` first
/// (2771-2775), then IBPB on VM exit given `IBPB_BRTYPE` (2777-2781), which
/// VMSCAPE takes up (2861-2863); without either the microcode is missing
/// (2703). `srso_strings` 2634, 2639 and 2640; `vmscape_strings` 2822-2823.
#[test]
fn srso_user_kernel_no_mitigates_only_the_vm_exit() {
    let at = |bp_spec_reduce, sbpb| {
        let facts = Facts {
            cpuid_8000_0021_eax: MILAN_GUEST.cpuid_8000_0021_eax
                | CPUID_8000_0021_EAX_SRSO_USER_KERNEL_NO
                | bp_spec_reduce,
            ..native_milan(0x0a00_11d7, sbpb)
        };
        let d = decide(&facts).expect("decided");
        (d.srso, d.vmscape, line(&facts, Vuln::SpecRstackOverflow), line(&facts, Vuln::Vmscape))
    };
    let exit_to_user = "Mitigation: IBPB before exit to userspace";
    assert_eq!(
        at(CPUID_8000_0021_EAX_SRSO_BP_SPEC_REDUCE, true),
        (
            Some(Srso::BpSpecReduce),
            Vmscape::IbpbExitToUser,
            "Mitigation: Reduced Speculation".into(),
            exit_to_user.into()
        )
    );
    assert_eq!(
        at(0, true),
        (
            Some(Srso::IbpbOnVmexit),
            Vmscape::IbpbOnVmexit,
            "Mitigation: IBPB on VMEXIT only".into(),
            "Mitigation: IBPB on VMEXIT".into()
        )
    );
    assert_eq!(
        at(0, false),
        (
            Some(Srso::UcodeNeeded),
            Vmscape::IbpbExitToUser,
            "Vulnerable: No microcode".into(),
            exit_to_user.into()
        )
    );
}

/// A KABYLAKE guest, family 6 model 0x9E stepping 0xA, without
/// `ARCH_CAPABILITIES`: its blacklist row gives SRBDS (`common.c:1303`), whose
/// mitigation a guest cannot know (`bugs.c:691-692`, `srbds_strings` 636), and
/// no `MDS_NO` gives MDS (`common.c:1447-1449`), whose host SMT state a guest
/// cannot know (`bugs.c:3117-3119`).
#[test]
fn an_intel_guest_cannot_know_its_srbds_mitigation_or_its_hosts_smt() {
    let guest = Facts {
        signature: 0x0009_06ea,
        cpuid_1_ecx: T14.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR,
        arch_capabilities: 0,
        mcu_opt_ctrl: None,
        ..T14
    };
    assert_eq!(line(&guest, Vuln::Srbds), "Unknown: Dependent on hypervisor status");
    assert_eq!(line(&guest, Vuln::Mds), "Mitigation: Clear CPU buffers; SMT Host state unknown");
}

#[test]
#[should_panic(expected = "PRED_CMD.SBPB is probed exactly where")]
fn a_native_zen3_without_its_probe_is_a_contradiction() {
    let _ = decide(&Facts { cpuid_1_ecx: 0, ..MILAN_GUEST });
}

#[test]
fn hygon_is_refused() {
    let hygon = Facts { vendor: Vendor::from_id(b"HygonGenuine"), ..TCG };
    assert_eq!(decide(&hygon).err(), Some(Refused::Hygon));
}
