//! Facts in, Linux's lines and selection out, the lines judged against
//! `grep .` over the vulnerabilities directory. Every expected value below the
//! two captures is read from the tag's source at the lines each test cites.

extern crate std;

use std::string::{String, ToString};
use std::vec::Vec;

use super::*;

const PREFIX: &str = "/sys/devices/system/cpu/vulnerabilities/";

/// The T14's i5-1135G7, held to the pinned Linux's reading of it by
/// [`the_t14s_facts_are_linuxs_reading_of_it`].
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
    // `fixtures/t14/` holds no 0x3A. `cpuinfo.txt` lists `vmx` and its
    // `vmx flags`, so Linux kept VMX (`feat_ctl.c:171-184`): these are the two
    // bits that decide that, and no other bit is read.
    feat_ctl: Some(FEAT_CTL_LOCKED | FEAT_CTL_VMX_ENABLED_OUTSIDE_SMX),
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    // `siblings 8`, `cpu cores 4`.
    smt: true,
};

/// The TCG model: `qemu64`, `AuthenticAMD` family 0xF model 0x6B stepping 1;
/// the bits `decide` reads in leaves 7 and 0x80000008 are clear in that model,
/// and it has no leaf 0x80000021.
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
    feat_ctl: None,
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
    feat_ctl: None,
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
    feat_ctl: None,
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    smt: false,
};

/// [`Decision`]'s selection, every public field: the destructuring in
/// [`state`] names each, so a field added there fails to build here.
#[derive(Debug, PartialEq)]
struct State {
    tsx: Tsx,
    spectre_v2: Option<SpectreV2>,
    bhi: Option<Bhi>,
    ibpb: bool,
    ibrs_fw: bool,
    stibp: Stibp,
    retbleed: Retbleed,
    ssb: Ssb,
    mds: Option<Mds>,
    taa: Option<Taa>,
    mmio: Option<Mmio>,
    rfds: Option<Rfds>,
    srbds: Option<Srbds>,
    gds: Option<Gds>,
    its: bool,
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
        vmx: _,
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
    spectre_v2: None,
    bhi: None,
    ibpb: false,
    ibrs_fw: false,
    stibp: Stibp::None,
    retbleed: Retbleed::None,
    ssb: Ssb::None,
    mds: None,
    taa: None,
    mmio: None,
    rfds: None,
    srbds: None,
    gds: None,
    its: false,
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
    let d = decide(facts);
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

fn line(facts: &Facts, v: Vuln) -> String {
    decide(facts).line(v).expect("modelled").to_string()
}

#[test]
fn the_t14s_facts_give_its_lines_and_its_spec_ctrl() {
    let d = assert_capture(&T14, include_str!("../fixtures/t14.txt"));
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Eibrs),
            bhi: Some(Bhi::SwLoop),
            ibpb: true,
            ssb: Ssb::Prctl,
            gds: Some(Gds::Full),
            its: true,
            spec_ctrl: 0x1,
            ..NONE
        }
    );
}

/// [`T14`] against `fixtures/t14/`, each field read as [`Facts`] says Linux
/// reads it, and what `decide` makes of it against the VMX Linux kept and the
/// `IA32_SPEC_CTRL` it left.
#[test]
fn the_t14s_facts_are_linuxs_reading_of_it() {
    let hex = |word: &str| u64::from_str_radix(word.trim_start_matches("0x"), 16).expect(word);
    let leaves: Vec<Vec<u64>> = include_str!("../fixtures/t14/cpuid.txt")
        .lines()
        .map(|l| l.split(' ').map(hex).collect())
        .collect();
    // A leaf past its range's highest reads as 0, as Linux leaves it.
    let leaf = |leaf: u32, subleaf: u32| {
        let read = |leaf: u32, subleaf: u32| {
            let l = leaves
                .iter()
                .find(|l| l[..2] == [u64::from(leaf), u64::from(subleaf)])
                .unwrap_or_else(|| panic!("CPUID {leaf:#x}.{subleaf} was not read"));
            [l[2], l[3], l[4], l[5]].map(|r| r as u32)
        };
        if leaf <= read(leaf & 0x8000_0000, 0)[0] { read(leaf, subleaf) } else { [0; 4] }
    };
    let blocks: Vec<Vec<(&str, &str)>> = include_str!("../fixtures/t14/cpuinfo.txt")
        .split_terminator("\n\n")
        .map(|b| {
            b.lines().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect()
        })
        .collect();
    let cpuinfo = |key: &str| {
        let mut each = blocks.iter().map(|b| b.iter().find(|f| f.0 == key).expect(key).1);
        let first = each.next().expect("a CPU");
        assert!(each.all(|v| v == first), "{key} differs between CPUs");
        first
    };
    let msrs: Vec<(u64, usize, u64)> = include_str!("../fixtures/t14/msr.txt")
        .lines()
        .map(|l| match l.split(' ').collect::<Vec<_>>()[..] {
            [index, cpu, value] => (hex(index), cpu.parse().expect(cpu), hex(value)),
            _ => panic!("{l:?} is not `msr cpu value`"),
        })
        .collect();
    let msr = |index: u64| {
        let each: Vec<_> = msrs.iter().filter(|m| m.0 == index).collect();
        assert!(each.iter().map(|m| m.1).eq(0..blocks.len()), "MSR {index:#x} on every CPU");
        assert!(each.iter().all(|m| m.2 == each[0].2), "MSR {index:#x} differs between CPUs");
        each[0].2
    };

    let [_, ebx0, ecx0, edx0] = leaf(0, 0);
    let id: Vec<u8> = [ebx0, edx0, ecx0].iter().flat_map(|r| r.to_le_bytes()).collect();
    let [eax1, _, ecx1, _] = leaf(1, 0);
    let [_, ebx7, _, edx7] = leaf(7, 0);
    let [eax21, _, ecx21, _] = leaf(0x8000_0021, 0);
    let arch = if edx7 & CPUID_7_0_EDX_ARCH_CAPABILITIES != 0 { msr(0x10a) } else { 0 };
    let Facts {
        vendor,
        signature,
        microcode,
        cpuid_1_ecx,
        cpuid_7_0_ebx,
        cpuid_7_0_edx,
        cpuid_7_2_edx,
        cpuid_8000_0008_ebx,
        cpuid_8000_0021_eax,
        cpuid_8000_0021_ecx,
        arch_capabilities,
        mcu_opt_ctrl,
        // Stands in for 0x3A, which was not read: `decide`'s `vmx` is held below.
        feat_ctl: _,
        // Probed on AMD and Hygon alone.
        ls_cfg_readable: _,
        sbpb_write_accepted: _,
        smt,
    } = T14;
    assert_eq!(cpuinfo("vendor_id").as_bytes(), &id[..]);
    assert_eq!(vendor, Vendor::from_id(id[..].try_into().expect("12 bytes")));
    assert_eq!(signature, eax1);
    let ident = Ident::new(vendor, signature);
    assert_eq!(
        [ident.family, ident.model, ident.stepping].map(|n| n.to_string()),
        ["cpu family", "model", "stepping"].map(|key| cpuinfo(key).to_string())
    );
    assert_eq!(u64::from(microcode), hex(cpuinfo("microcode")));
    assert_eq!(u64::from(microcode), msr(0x8b) >> 32);
    assert_eq!(cpuid_1_ecx, ecx1);
    assert_eq!((cpuid_7_0_ebx, cpuid_7_0_edx), (ebx7, edx7));
    assert_eq!(cpuid_7_2_edx, leaf(7, 2)[3]);
    assert_eq!(cpuid_8000_0008_ebx, leaf(0x8000_0008, 0)[1]);
    assert_eq!((cpuid_8000_0021_eax, cpuid_8000_0021_ecx), (eax21, ecx21));
    assert_eq!(arch_capabilities, arch);
    assert_eq!(mcu_opt_ctrl, (arch & ARCH_CAP_GDS_CTRL != 0).then(|| msr(0x123)));
    let count = |key| cpuinfo(key).parse::<u32>().expect(key);
    assert_eq!(smt, count("siblings") > count("cpu cores"));

    let d = decide(&T14);
    assert_eq!(d.vmx, cpuinfo("flags").split(' ').any(|f| f == "vmx"));
    assert_eq!(d.spec_ctrl, msr(0x48));
}

/// `fixtures/tcg/console.txt` verifies the TCG model's signature and nothing
/// else it could read, so its lines are held over either value of every other
/// fact they could rest on.
#[test]
fn the_tcg_models_signature_gives_its_lines() {
    let console = include_str!("../fixtures/tcg/console.txt");
    // `print_cpu_info`: the vendor as its `cpu_dev` names it, the brand string,
    // then the family, model and stepping.
    let (name, identity) = console
        .lines()
        .find_map(|l| l.split_once("smpboot: CPU0: "))
        .and_then(|(_, cpu0)| cpu0.split_once(" ("))
        .expect("Linux names CPU 0");
    let id = Ident::new(TCG.vendor, TCG.signature);
    assert_eq!((TCG.vendor, name.split(' ').next()), (Vendor::Amd, Some("AMD")));
    assert_eq!(
        identity,
        std::format!("family: {:#x}, model: {:#x}, stepping: {:#x})", id.family, id.model, id.stepping)
    );
    let lines: String =
        console.lines().filter(|l| l.starts_with(PREFIX)).flat_map(|l| [l, "\n"]).collect();
    for hypervisor in [0, CPUID_1_ECX_HYPERVISOR] {
        for rdrand in [0, CPUID_1_ECX_RDRAND] {
            for microcode in [0, u32::MAX] {
                for smt in [false, true] {
                    let facts =
                        Facts { cpuid_1_ecx: hypervisor | rdrand, microcode, smt, ..TCG };
                    let d = assert_capture(&facts, &lines);
                    assert_eq!(
                        state(&d),
                        State { spectre_v2: Some(SpectreV2::Retpoline), ..NONE }
                    );
                }
            }
        }
    }
}

/// The track's exit control: the T14 with `IA32_ARCH_CAPABILITIES` read as 0.
/// Without `IBRS_ALL` it is in retpoline mode, without `GDS_CTRL` it has no GDS
/// microcode, without `MDS_NO` it clears CPU buffers, without `PSCHANGE_MC_NO`
/// its kept VMX gives ITLB multihit's "VMX disabled" (`bugs.c:3096-3097`), and
/// without `RDCL_NO` Linux's L1TF line turns on state the facts do not carry.
#[test]
fn the_t14_with_arch_capabilities_read_as_zero() {
    let d = decide(&Facts { arch_capabilities: 0, mcu_opt_ctrl: None, ..T14 });
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
    // Without SMT, "disabled" (`bugs.c:3128-3129`).
    let no_smt = Facts { arch_capabilities: 0, mcu_opt_ctrl: None, smt: false, ..T14 };
    assert_eq!(
        decide(&no_smt).line(Vuln::Mds).expect("modelled").to_string(),
        "Mitigation: Clear CPU buffers; SMT disabled"
    );
    assert_eq!(line(Vuln::L1tf).err(), Some(Unmodelled::L1tf));
    assert_eq!(line(Vuln::ItlbMultihit).as_deref(), Ok("KVM: Mitigation: VMX disabled"));
    assert_eq!(line(Vuln::TsxAsyncAbort).as_deref(), Ok("Not affected"));
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Retpoline),
            ibpb: true,
            ibrs_fw: true,
            stibp: Stibp::Prctl,
            ssb: Ssb::Prctl,
            mds: Some(Mds::Full),
            gds: Some(Gds::UcodeNeeded),
            its: true,
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

/// COMETLAKE_L stepping 0 has its own row without MMIO_SBDS and GDS, ahead of
/// its any-stepping row that carries both (`common.c:1310-1311`); SRBDS and GDS
/// are forced by those flags (`common.c:1480-1483`, `1521-1523`).
#[test]
fn a_blacklist_row_for_stepping_zero_stands_before_its_any_stepping_row() {
    let at = |stepping: u32| decide(&Facts { signature: 0x000a_0660 | stepping, ..COMETLAKE });
    let d = at(0);
    assert_eq!((d.srbds, d.gds), (None, None));
    let d = at(1);
    assert_eq!((d.srbds, d.gds), (Some(Srbds::Full), Some(Gds::UcodeNeeded)));
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

/// KABYLAKE (family 6 model 0x9E) stepping 0xA with [`T14`]'s CPUID and no
/// `ARCH_CAPABILITIES`, outside a hypervisor.
const KABYLAKE: Facts = Facts { signature: 0x0009_06ea, arch_capabilities: 0, mcu_opt_ctrl: None, ..T14 };

/// KABYLAKE stepping 0xA's bad microcode is 0x80 and below
/// (`intel.c:143,177-179`): there Linux drops IBRS, IBPB, STIBP and SSBD
/// (`intel.c:296-309`), so Spectre v2 falls to retpolines (`bugs.c:1884-1893`),
/// RETBleed to "Vulnerable" (`bugs.c:1175-1190`), STIBP to none
/// (`bugs.c:1564-1567`) and VMSCAPE to "Vulnerable" (`bugs.c:2851-2855`).
#[test]
fn spectre_bad_microcode_includes_its_bound() {
    let d = decide(&Facts { microcode: 0x80, ..KABYLAKE });
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Retpoline),
            mds: Some(Mds::Full),
            mmio: Some(Mmio::Verw),
            srbds: Some(Srbds::Full),
            gds: Some(Gds::UcodeNeeded),
            clear_cpu_buf: true,
            ..NONE
        }
    );
    let line = |v| d.line(v).expect("modelled").to_string();
    assert_eq!(
        line(Vuln::SpectreV2),
        "Mitigation: Retpolines; STIBP: disabled; RSB filling; PBRSB-eIBRS: Not affected; \
         BHI: Not affected"
    );
    assert_eq!(line(Vuln::Retbleed), "Vulnerable");
    assert_eq!(line(Vuln::SpecStoreBypass), "Vulnerable");
    assert_eq!(line(Vuln::Vmscape), "Vulnerable");
    let d = decide(&Facts { microcode: 0x81, ..KABYLAKE });
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Ibrs),
            ibpb: true,
            stibp: Stibp::Prctl,
            retbleed: Retbleed::Ibrs,
            ssb: Ssb::Prctl,
            mds: Some(Mds::Full),
            mmio: Some(Mmio::Verw),
            srbds: Some(Srbds::Full),
            gds: Some(Gds::UcodeNeeded),
            vmscape: Vmscape::IbpbExitToUser,
            clear_cpu_buf: true,
            spec_ctrl: SPEC_CTRL_IBRS,
            ..NONE
        }
    );
    assert_eq!(
        d.line(Vuln::SpectreV2).expect("modelled").to_string(),
        "Mitigation: IBRS; IBPB: conditional; STIBP: conditional; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: Not affected"
    );
    assert_eq!(d.line(Vuln::Retbleed).expect("modelled").to_string(), "Mitigation: IBRS");
}

/// `early_init_intel` drops the controls only where one of IBRS, IBPB or STIBP
/// is set (`intel.c:296-299`; `init_speculation_control` sets them from the
/// AMD bits too, `common.c:994-1005`): alone, `SPEC_CTRL_SSBD` keeps SSBD.
#[test]
fn bad_microcode_drops_the_controls_only_beside_one_of_them() {
    let ssb = |ebx8| {
        decide(&Facts {
            microcode: 0x80,
            cpuid_7_0_edx: CPUID_7_0_EDX_SPEC_CTRL_SSBD,
            cpuid_8000_0008_ebx: ebx8,
            ..KABYLAKE
        })
        .ssb
    };
    assert_eq!(ssb(0), Ssb::Prctl);
    assert_eq!(ssb(CPUID_8000_0008_EBX_AMD_IBRS), Ssb::None);
    assert_eq!(ssb(CPUID_8000_0008_EBX_AMD_IBPB), Ssb::None);
    assert_eq!(ssb(CPUID_8000_0008_EBX_AMD_STIBP), Ssb::None);
}

/// `spectre_bad_microcodes` keys `x86_vfm`, vendor and family with the model
/// (`intel.c:141-143,177`), and only `early_init_intel` reads it: neither an
/// unknown vendor's nor an Intel family 0xF's model 0x9E stepping 0xA drops
/// the controls at microcode 0x80.
#[test]
fn bad_microcode_is_intel_family_6s() {
    let unknown = Facts { vendor: Vendor::from_id(b"GenuineIotel"), feat_ctl: None, ..KABYLAKE };
    let family_f = Facts { signature: 0x0009_0fea, ..KABYLAKE };
    assert_eq!(Ident::new(family_f.vendor, family_f.signature).model, 0x9E);
    for facts in [unknown, family_f] {
        let d = decide(&Facts { microcode: 0x80, ..facts });
        assert_eq!((d.ibpb, d.ssb), (true, Ssb::Prctl), "{:?}", facts.vendor);
    }
}

#[test]
fn a_guest_cannot_know_its_gds_mitigation() {
    let guest = Facts { cpuid_1_ecx: T14.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR, ..T14 };
    assert_eq!(line(&guest, Vuln::GatherDataSampling), "Unknown: Dependent on hypervisor status");
}

/// `gds_select_mitigation` reads `GDS_MITG_LOCKED` (`bugs.c:849-861`); `gds_strings`
/// 766.
#[test]
fn firmware_can_lock_the_gds_mitigation() {
    let locked = Facts { mcu_opt_ctrl: Some(GDS_MITG_LOCKED), ..T14 };
    assert_eq!(decide(&locked).gds, Some(Gds::FullLocked));
    assert_eq!(line(&locked, Vuln::GatherDataSampling), "Mitigation: Microcode (locked)");
}

/// `RTM_ALWAYS_ABORT` sets `TSX_CTRL_RTM_ALWAYS_ABORT` before `TSX_CTRL` is
/// looked at (`tsx.c:170-176`).
#[test]
fn rtm_always_abort_decides_tsx_first() {
    let edx7 = T14.cpuid_7_0_edx | CPUID_7_0_EDX_RTM_ALWAYS_ABORT;
    let d = decide(&Facts { cpuid_7_0_edx: edx7, ..T14 });
    assert_eq!(d.tsx, Tsx::RtmAlwaysAbort);
    let arch = T14.arch_capabilities | ARCH_CAP_TSX_CTRL_MSR;
    assert_eq!(decide(&Facts { cpuid_7_0_edx: edx7, arch_capabilities: arch, ..T14 }).tsx, Tsx::RtmAlwaysAbort);
    assert_eq!(decide(&Facts { arch_capabilities: arch, ..T14 }).tsx, Tsx::Disable);
}

// The two fixtures below are this crate's reading of the pinned Linux, awaiting
// a capture on a nightly runner:
// `issues/build/no-nightly-runner-has-had-its-cpuid-and-vulnerability-lines-captured.md`.

/// A guest's lines do not read its microcode: `tsa_init` returns under a
/// hypervisor (`amd.c:519-520`) before `amd_check_tsa_microcode`.
#[test]
fn a_milan_kvm_guest_gives_the_tags_lines() {
    for microcode in [0, u32::MAX] {
        let d = assert_capture(
            &Facts { microcode, ..MILAN_GUEST },
            include_str!("../fixtures/awaiting-capture/milan-kvm.txt"),
        );
        assert_eq!(
            state(&d),
            State {
                spectre_v2: Some(SpectreV2::Retpoline),
                ibpb: true,
                ibrs_fw: true,
                ssb: Ssb::Prctl,
                srso: Some(Srso::SafeRetUcodeNeeded),
                tsa: Some(Tsa::UcodeNeeded),
                clear_cpu_buf: true,
                ..NONE
            }
        );
    }
}

#[test]
fn a_turin_kvm_guest_gives_the_tags_lines() {
    let d = assert_capture(&TURIN_GUEST, include_str!("../fixtures/awaiting-capture/turin-kvm.txt"));
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Eibrs),
            ibpb: true,
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
    let d = decide(&rome(false));
    assert_eq!(
        d.line(Vuln::SpecRstackOverflow).expect("modelled").to_string(),
        "Mitigation: SMT disabled"
    );
    assert_eq!(
        d.line(Vuln::Retbleed).expect("modelled").to_string(),
        "Mitigation: untrained return thunk; SMT disabled"
    );
    let d = decide(&rome(true));
    assert_eq!(
        d.line(Vuln::Retbleed).expect("modelled").to_string(),
        "Mitigation: untrained return thunk; SMT enabled with STIBP protection"
    );
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Retpoline),
            ibpb: true,
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

/// A native Zen1, family 0x17 model 1 stepping 1 (`amd.c:602-606`), with SMT
/// and no leaf 0x80000021; 0x80000008:EBX carries AMD_IBPB, AMD_IBRS and
/// AMD_SSBD, and not AMD_STIBP_ALWAYS_ON.
const ZEN1: Facts = Facts {
    vendor: Vendor::from_id(b"AuthenticAMD"),
    signature: 0x0080_0f11,
    microcode: 0,
    cpuid_1_ecx: 0,
    cpuid_7_0_ebx: 0,
    cpuid_7_0_edx: 0,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: CPUID_8000_0008_EBX_AMD_IBPB
        | CPUID_8000_0008_EBX_AMD_IBRS
        | CPUID_8000_0008_EBX_AMD_SSBD,
    cpuid_8000_0021_eax: 0,
    cpuid_8000_0021_ecx: 0,
    arch_capabilities: 0,
    mcu_opt_ctrl: None,
    feat_ctl: None,
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    smt: true,
};

/// Zen1 with SMT: the untrained return thunk (`bugs.c:1103-1106`) forces STIBP
/// always on without AMD_STIBP_ALWAYS_ON (`bugs.c:1579-1585`), which sets
/// `SPEC_CTRL.STIBP` (`bugs.c:2054-2068,2976-2977`) and gives retbleed's "SMT
/// enabled with STIBP protection" (`bugs.c:3271-3275`); IBPB-extending microcode
/// from `IBPB_BRTYPE` (`amd.c:799-801`) gives safe RET (`bugs.c:2733-2734`).
/// Without AMD_STIBP there is no STIBP to force (`bugs.c:1564-1567`) and
/// retbleed's SMT is "vulnerable".
#[test]
fn zen1_forces_stibp_from_the_untrained_return_thunk() {
    let with = Facts { cpuid_8000_0008_ebx: ZEN1.cpuid_8000_0008_ebx | CPUID_8000_0008_EBX_AMD_STIBP, ..ZEN1 };
    let d = decide(&with);
    let expected = State {
        spectre_v2: Some(SpectreV2::Retpoline),
        ibpb: true,
        stibp: Stibp::StrictPreferred,
        retbleed: Retbleed::Unret,
        ssb: Ssb::Prctl,
        vmscape: Vmscape::IbpbExitToUser,
        srso: Some(Srso::SafeRet),
        spec_ctrl: SPEC_CTRL_STIBP,
        ..NONE
    };
    assert_eq!(state(&d), expected);
    assert_eq!(
        line(&with, Vuln::SpectreV2),
        "Mitigation: Retpolines; IBPB: conditional; STIBP: always-on; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: Not affected"
    );
    assert_eq!(
        line(&with, Vuln::Retbleed),
        "Mitigation: untrained return thunk; SMT enabled with STIBP protection"
    );
    assert_eq!(line(&with, Vuln::SpecRstackOverflow), "Mitigation: Safe RET");
    let d = decide(&ZEN1);
    assert_eq!(state(&d), State { stibp: Stibp::None, spec_ctrl: 0, ..expected });
    assert_eq!(
        line(&ZEN1, Vuln::SpectreV2),
        "Mitigation: Retpolines; IBPB: conditional; STIBP: disabled; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: Not affected"
    );
    assert_eq!(line(&ZEN1, Vuln::Retbleed), "Mitigation: untrained return thunk; SMT vulnerable");
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
    spectre_v2: Some(SpectreV2::Retpoline),
    ibpb: true,
    ibrs_fw: true,
    ssb: Ssb::Prctl,
    vmscape: Vmscape::IbpbExitToUser,
    ..NONE
};

/// A Milan outside a hypervisor: its `PRED_CMD.SBPB` probe decides SRSO's
/// microcode, and `amd_check_tsa_microcode`'s row for 0xA0011 decides TSA's.
#[test]
fn zen3_native_reads_its_probe_and_its_tsa_microcode() {
    let d = decide(&native_milan(0x0a00_11d6, false));
    assert_eq!(
        state(&d),
        State {
            srso: Some(Srso::SafeRetUcodeNeeded),
            tsa: Some(Tsa::UcodeNeeded),
            ..NATIVE_MILAN
        }
    );
    let d = decide(&native_milan(0x0a00_11d7, true));
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
    let d = decide(&genoa(0x0a10_114c));
    assert_eq!((d.tsa, d.clear_cpu_buf), (Some(Tsa::Full), true));
    assert_eq!(line(&genoa(0x0a10_114c), Vuln::Tsa), "Mitigation: Clear CPU buffers");
    let d = decide(&genoa(0x0a10_114b));
    assert_eq!((d.tsa, d.clear_cpu_buf), (Some(Tsa::UcodeNeeded), false));
    assert_eq!(
        line(&genoa(0x0a10_114b), Vuln::Tsa),
        "Vulnerable: Clear CPU buffers attempted, no microcode"
    );
}

/// A Zen3 model 0x08 stepping 2 (signature `0x00a00f82`) keys `amd_check_tsa_microcode`'s
/// row 0xa0082 by the whole model byte (`amd.c:479-490`, `ZEN3` from `bsp_init_amd`, `amd.c:621`):
/// microcode 0x0a00820d and up is mitigated, below it is missing.
#[test]
fn a_tsa_row_keyed_by_a_model_nibble_of_8_or_more_is_read() {
    let at = |microcode| Facts { signature: 0x00a0_0f82, ..native_milan(microcode, true) };
    assert_eq!(decide(&at(0x0a00_820d)).tsa, Some(Tsa::Full));
    assert_eq!(decide(&at(0x0a00_820c)).tsa, Some(Tsa::UcodeNeeded));
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
    let d = decide(&milan);
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
        let d = decide(&at(true));
        assert_eq!(d.ssb, Ssb::Prctl, "{signature:#x}");
        assert_eq!(
            line(&at(true), Vuln::SpecStoreBypass),
            "Mitigation: Speculative Store Bypass disabled via prctl"
        );
        let d = decide(&at(false));
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
        let d = decide(&facts);
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

/// [`KABYLAKE`] as a guest at its bad microcode, which a guest does not check
/// (`intel.c:173-174`): the hypervisor bit alone gives BHI (`common.c:1533-1536`)
/// and ITS (`common.c:1405-1406`; this stepping's row, `common.c:1303`, has no
/// ITS). IBRS stays, so Spectre v2 is IBRS (`bugs.c:1884-1891`) with the SW
/// BHB loop (`bugs.c:1851-1857`); SRBDS's and GDS's mitigations a guest cannot
/// know (`bugs.c:691-692,821-824`), nor its host's SMT (`bugs.c:3117-3119`,
/// 3155-3157).
#[test]
fn an_intel_guest_cannot_know_its_mitigations_or_its_hosts_smt() {
    let guest = Facts {
        microcode: 0x80,
        cpuid_1_ecx: T14.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR,
        ..KABYLAKE
    };
    let d = decide(&guest);
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Ibrs),
            bhi: Some(Bhi::SwLoop),
            ibpb: true,
            stibp: Stibp::Prctl,
            retbleed: Retbleed::Ibrs,
            ssb: Ssb::Prctl,
            mds: Some(Mds::Full),
            mmio: Some(Mmio::Verw),
            srbds: Some(Srbds::Hypervisor),
            gds: Some(Gds::Hypervisor),
            its: true,
            clear_cpu_buf: true,
            spec_ctrl: SPEC_CTRL_IBRS,
            ..NONE
        }
    );
    let line = |v| d.line(v).expect("modelled").to_string();
    assert_eq!(
        line(Vuln::SpectreV2),
        "Mitigation: IBRS; IBPB: conditional; STIBP: conditional; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: SW loop, KVM: SW loop"
    );
    assert_eq!(line(Vuln::Srbds), "Unknown: Dependent on hypervisor status");
    assert_eq!(line(Vuln::Mds), "Mitigation: Clear CPU buffers; SMT Host state unknown");
    assert_eq!(line(Vuln::MmioStaleData), "Mitigation: Clear CPU buffers; SMT Host state unknown");
    assert_eq!(line(Vuln::IndirectTargetSelection), "Mitigation: Aligned branch/return thunks");
    assert_eq!(line(Vuln::Vmscape), "Not affected");
}

/// An Alder Lake (family 6 model 0x97) with [`T14`]'s CPUID and
/// `ARCH_CAPABILITIES` and with `BHI_CTRL`: eIBRS sets `SPEC_CTRL.IBRS`
/// (`bugs.c:1930-1936`) and BHI is mitigated in hardware by `BHI_DIS_S`
/// (`bugs.c:1795-1805,1847-1849`; `spectre_bhi_state` 3224-3225). STIBP is not
/// printed under eIBRS (`bugs.c:3176-3178`); its row gives VMSCAPE
/// (`common.c:1316`).
#[test]
fn alder_lake_mitigates_bhi_in_hardware() {
    let adl = Facts {
        signature: 0x0009_0672,
        cpuid_7_2_edx: T14.cpuid_7_2_edx | CPUID_7_2_EDX_BHI_CTRL,
        ..T14
    };
    let d = decide(&adl);
    assert_eq!(
        state(&d),
        State {
            spectre_v2: Some(SpectreV2::Eibrs),
            bhi: Some(Bhi::BhiDisS),
            ibpb: true,
            ssb: Ssb::Prctl,
            vmscape: Vmscape::IbpbExitToUser,
            spec_ctrl: SPEC_CTRL_IBRS | SPEC_CTRL_BHI_DIS_S,
            ..NONE
        }
    );
    assert_eq!(
        line(&adl, Vuln::SpectreV2),
        "Mitigation: Enhanced / Automatic IBRS; IBPB: conditional; PBRSB-eIBRS: SW sequence; \
         BHI: BHI_DIS_S"
    );
    assert_eq!(line(&adl, Vuln::IndirectTargetSelection), "Not affected");
}

/// [`T14`] as a guest without `IBRS_ALL` and with `RRSBA`: retpolines
/// (`bugs.c:1893`) disable RRSBA through `RRSBA_DIS_S` where `RRSBA_CTRL` is
/// (`bugs.c:1723-1739,1966-1969`), which leaves BHI to them
/// (`bugs.c:1837-1841`; `spectre_bhi_state` 3228-3231); without `RRSBA_CTRL`
/// BHI takes the SW loop (`bugs.c:1851-1857`).
#[test]
fn a_retpoline_guest_leaves_bhi_to_retpolines_only_with_rrsba_disabled() {
    let guest = |edx72| Facts {
        cpuid_1_ecx: T14.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR,
        cpuid_7_2_edx: edx72,
        arch_capabilities: (T14.arch_capabilities & !ARCH_CAP_IBRS_ALL) | ARCH_CAP_RRSBA,
        ..T14
    };
    let with = guest(T14.cpuid_7_2_edx | CPUID_7_2_EDX_RRSBA_CTRL);
    let expected = State {
        spectre_v2: Some(SpectreV2::Retpoline),
        bhi: Some(Bhi::Retpoline),
        ibpb: true,
        ibrs_fw: true,
        stibp: Stibp::Prctl,
        ssb: Ssb::Prctl,
        gds: Some(Gds::Hypervisor),
        its: true,
        spec_ctrl: SPEC_CTRL_RRSBA_DIS_S,
        ..NONE
    };
    assert_eq!(state(&decide(&with)), expected);
    assert_eq!(
        line(&with, Vuln::SpectreV2),
        "Mitigation: Retpolines; IBPB: conditional; IBRS_FW; STIBP: conditional; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: Retpoline"
    );
    let without = guest(T14.cpuid_7_2_edx);
    assert_eq!(state(&decide(&without)), State { bhi: Some(Bhi::SwLoop), spec_ctrl: 0, ..expected });
    assert_eq!(
        line(&without, Vuln::SpectreV2),
        "Mitigation: Retpolines; IBPB: conditional; IBRS_FW; STIBP: conditional; RSB filling; \
         PBRSB-eIBRS: Not affected; BHI: SW loop, KVM: SW loop"
    );
}

/// A native Comet Lake (family 6 model 0xA5) with RTM and `MD_CLEAR`,
/// `FLUSH_L1D` and `SRBDS_CTRL`, whose `ARCH_CAPABILITIES` the fixtures below
/// vary; its row (`common.c:1309`) gives MMIO, MMIO_SBDS, RETBLEED, GDS, ITS
/// and VMSCAPE.
const COMETLAKE: Facts = Facts {
    vendor: Vendor::from_id(b"GenuineIntel"),
    signature: 0x000a_0655,
    microcode: 0xf8,
    cpuid_1_ecx: CPUID_1_ECX_VMX | CPUID_1_ECX_AVX | CPUID_1_ECX_RDRAND,
    cpuid_7_0_ebx: CPUID_7_0_EBX_RTM | CPUID_7_0_EBX_RDSEED,
    cpuid_7_0_edx: CPUID_7_0_EDX_SRBDS_CTRL
        | CPUID_7_0_EDX_MD_CLEAR
        | CPUID_7_0_EDX_SPEC_CTRL
        | CPUID_7_0_EDX_INTEL_STIBP
        | CPUID_7_0_EDX_FLUSH_L1D
        | CPUID_7_0_EDX_ARCH_CAPABILITIES
        | CPUID_7_0_EDX_SPEC_CTRL_SSBD,
    cpuid_7_2_edx: 0,
    cpuid_8000_0008_ebx: 0,
    cpuid_8000_0021_eax: 0,
    cpuid_8000_0021_ecx: 0,
    arch_capabilities: 0,
    mcu_opt_ctrl: None,
    feat_ctl: Some(FEAT_CTL_LOCKED | FEAT_CTL_VMX_ENABLED_OUTSIDE_SMX),
    ls_cfg_readable: None,
    sbpb_write_accepted: None,
    smt: true,
};

/// What [`COMETLAKE`] selects outside its buffer-clearing bugs: IBRS for
/// RETBleed (`bugs.c:1884-1891,1175-1178`), no GDS microcode
/// (`bugs.c:831-842`), ITS's thunks and VMSCAPE's IBPB.
const COMETLAKE_STATE: State = State {
    spectre_v2: Some(SpectreV2::Ibrs),
    ibpb: true,
    stibp: Stibp::Prctl,
    retbleed: Retbleed::Ibrs,
    ssb: Ssb::Prctl,
    gds: Some(Gds::UcodeNeeded),
    its: true,
    vmscape: Vmscape::IbpbExitToUser,
    spec_ctrl: SPEC_CTRL_IBRS,
    ..NONE
};

/// Without `MDS_NO`, `MD_CLEAR` mitigates TAA (`bugs.c:353-356`) and, with
/// `FLUSH_L1D`, MMIO (`bugs.c:468-472`). With `MDS_NO` and without
/// `TSX_CTRL_MSR` TAA's microcode is missing (`bugs.c:367-369`), and so is
/// MMIO's unless `FB_CLEAR` (`bugs.c:468-474`). RTM on keeps SRBDS's
/// microcode mitigation (`bugs.c:688-695`).
#[test]
fn comet_lake_clears_cpu_buffers_by_mds_no_and_fb_clear() {
    let at = |arch| Facts { arch_capabilities: arch, ..COMETLAKE };
    let d = decide(&at(0));
    assert_eq!(
        state(&d),
        State {
            mds: Some(Mds::Full),
            taa: Some(Taa::Verw),
            mmio: Some(Mmio::Verw),
            srbds: Some(Srbds::Full),
            clear_cpu_buf: true,
            ..COMETLAKE_STATE
        }
    );
    let line = |facts: &Facts, v| line(facts, v);
    assert_eq!(line(&at(0), Vuln::TsxAsyncAbort), "Mitigation: Clear CPU buffers; SMT vulnerable");
    assert_eq!(line(&at(0), Vuln::MmioStaleData), "Mitigation: Clear CPU buffers; SMT vulnerable");
    assert_eq!(line(&at(0), Vuln::Srbds), "Mitigation: Microcode");
    let mds_no = at(ARCH_CAP_MDS_NO);
    assert_eq!(
        state(&decide(&mds_no)),
        State {
            taa: Some(Taa::UcodeNeeded),
            mmio: Some(Mmio::UcodeNeeded),
            srbds: Some(Srbds::Full),
            clear_cpu_buf: true,
            ..COMETLAKE_STATE
        }
    );
    let no_microcode = "Vulnerable: Clear CPU buffers attempted, no microcode; SMT vulnerable";
    assert_eq!(line(&mds_no, Vuln::TsxAsyncAbort), no_microcode);
    assert_eq!(line(&mds_no, Vuln::MmioStaleData), no_microcode);
    assert_eq!(line(&mds_no, Vuln::Mds), "Not affected");
    let fb_clear = at(ARCH_CAP_MDS_NO | ARCH_CAP_FB_CLEAR);
    assert_eq!(decide(&fb_clear).mmio, Some(Mmio::Verw));
}

/// SRBDS is "TSX disabled" on an `MDS_NO` CPU only where RTM is off and MMIO
/// Stale Data is not (`bugs.c:688-690`): `TSX_CTRL` disables RTM under
/// `TSX_MODE_OFF` (`tsx.c:188-227`), as `RTM_ALWAYS_ABORT` does
/// (`tsx.c:170-176`), and TAA is then "TSX disabled" (`bugs.c:335-338`)
/// without clearing buffers; the three `*_NO` bits make MMIO immune
/// (`common.c:1363-1368,1495`).
#[test]
fn srbds_is_tsx_disabled_only_without_rtm_and_mmio() {
    let immune = ARCH_CAP_MDS_NO | ARCH_CAP_FBSDP_NO | ARCH_CAP_PSDP_NO | ARCH_CAP_SBDR_SSDP_NO;
    let tsx_ctrl = Facts { arch_capabilities: immune | ARCH_CAP_TSX_CTRL_MSR, ..COMETLAKE };
    let off = State {
        tsx: Tsx::Disable,
        taa: Some(Taa::TsxDisabled),
        srbds: Some(Srbds::TsxOff),
        ..COMETLAKE_STATE
    };
    assert_eq!(state(&decide(&tsx_ctrl)), off);
    assert_eq!(line(&tsx_ctrl, Vuln::Srbds), "Mitigation: TSX disabled");
    assert_eq!(line(&tsx_ctrl, Vuln::TsxAsyncAbort), "Mitigation: TSX disabled");
    assert_eq!(line(&tsx_ctrl, Vuln::MmioStaleData), "Not affected");
    let always_abort = Facts {
        cpuid_7_0_edx: COMETLAKE.cpuid_7_0_edx | CPUID_7_0_EDX_RTM_ALWAYS_ABORT,
        arch_capabilities: immune,
        ..COMETLAKE
    };
    assert_eq!(state(&decide(&always_abort)), State { tsx: Tsx::RtmAlwaysAbort, ..off });
    let rtm = Facts { arch_capabilities: immune, ..COMETLAKE };
    assert_eq!(
        state(&decide(&rtm)),
        State {
            taa: Some(Taa::UcodeNeeded),
            srbds: Some(Srbds::Full),
            clear_cpu_buf: true,
            ..COMETLAKE_STATE
        }
    );
    let mmio = Facts { arch_capabilities: ARCH_CAP_MDS_NO | ARCH_CAP_TSX_CTRL_MSR, ..COMETLAKE };
    assert_eq!(
        state(&decide(&mmio)),
        State {
            tsx: Tsx::Disable,
            taa: Some(Taa::TsxDisabled),
            mmio: Some(Mmio::UcodeNeeded),
            srbds: Some(Srbds::Full),
            ..COMETLAKE_STATE
        }
    );
}

/// An affected CPU's ITLB multihit line reads `X86_FEATURE_MSR_IA32_FEAT_CTL`
/// and `X86_FEATURE_VMX` (`bugs.c:3093-3095`) as `init_ia32_feat_ctl` left them:
/// a faulting read clears VMX (`feat_ctl.c:119-123`), an unlocked MSR is locked
/// with VMX enabled (`feat_ctl.c:139-166`), and a locked one keeps VMX only
/// with `VMX_ENABLED_OUTSIDE_SMX` (`feat_ctl.c:174-179`). No VM runs, so a kept
/// VMX is "disabled" (`bugs.c:3096-3097`).
#[test]
fn itlb_multihit_reads_what_feat_ctl_left_of_vmx() {
    let at = |ecx, feat_ctl| line(&Facts { cpuid_1_ecx: ecx, feat_ctl, ..COMETLAKE }, Vuln::ItlbMultihit);
    let ecx = COMETLAKE.cpuid_1_ecx;
    let (disabled, unsupported) = ("KVM: Mitigation: VMX disabled", "KVM: Mitigation: VMX unsupported");
    assert_eq!(at(ecx, Some(0)), disabled);
    assert_eq!(at(ecx, Some(FEAT_CTL_LOCKED | FEAT_CTL_VMX_ENABLED_OUTSIDE_SMX)), disabled);
    assert_eq!(at(ecx, Some(FEAT_CTL_LOCKED)), unsupported);
    assert_eq!(at(ecx, Some(FEAT_CTL_LOCKED | 1 << 1)), unsupported);
    assert_eq!(at(ecx, None), unsupported);
    assert_eq!(at(ecx & !CPUID_1_ECX_VMX, Some(0)), unsupported);
    assert_eq!(line(&COMETLAKE, Vuln::ItlbMultihit), disabled);
    assert_eq!(
        line(&Facts { arch_capabilities: ARCH_CAP_PSCHANGE_MC_NO, ..COMETLAKE }, Vuln::ItlbMultihit),
        "Not affected"
    );
}

#[test]
#[should_panic(expected = "IA32_FEAT_CTL is read only where")]
fn an_amd_cpu_with_feat_ctl_is_a_contradiction() {
    decide(&Facts { feat_ctl: Some(0), ..TCG });
}

/// A Silvermont (family 6 model 0x37) is `MSBDS_ONLY` (`common.c:1202`), whose
/// SMT line is "mitigated" or "disabled" (`bugs.c:3122-3126`). Neither MMIO
/// row names it, so its MMIO is unknown (`common.c:1498-1499`).
#[test]
fn an_msbds_only_cpu_reports_its_smt_as_mitigated() {
    let silvermont = |smt| Facts {
        vendor: Vendor::from_id(b"GenuineIntel"),
        signature: 0x0003_0678,
        microcode: 0,
        cpuid_1_ecx: 0,
        cpuid_7_0_ebx: 0,
        cpuid_7_0_edx: CPUID_7_0_EDX_MD_CLEAR | CPUID_7_0_EDX_SPEC_CTRL | CPUID_7_0_EDX_INTEL_STIBP,
        cpuid_7_2_edx: 0,
        cpuid_8000_0008_ebx: 0,
        cpuid_8000_0021_eax: 0,
        cpuid_8000_0021_ecx: 0,
        arch_capabilities: 0,
        mcu_opt_ctrl: None,
        feat_ctl: None,
        ls_cfg_readable: None,
        sbpb_write_accepted: None,
        smt,
    };
    assert_eq!(line(&silvermont(true), Vuln::Mds), "Mitigation: Clear CPU buffers; SMT mitigated");
    assert_eq!(line(&silvermont(false), Vuln::Mds), "Mitigation: Clear CPU buffers; SMT disabled");
    assert_eq!(line(&silvermont(true), Vuln::MmioStaleData), "Unknown: No mitigations");
    assert_eq!(line(&silvermont(true), Vuln::L1tf), "Not affected");
    assert_eq!(
        state(&decide(&silvermont(true))),
        State {
            spectre_v2: Some(SpectreV2::Retpoline),
            ibpb: true,
            ibrs_fw: true,
            stibp: Stibp::Prctl,
            mds: Some(Mds::Full),
            clear_cpu_buf: true,
            ..NONE
        }
    );
}

/// A native Hygon Dhyana, family 0x18 model 0 stepping 1, with SMT: its
/// whitelist row is AMD's (`common.c:1238`) and its blacklist row gives
/// RETBLEED, SRSO and VMSCAPE (`common.c:1341`). Hygon probes `LS_CFG` for
/// SSBD in every family (`hygon.c:228-239`), takes the untrained return thunk
/// (`bugs.c:1103-1106`) and IBPB rather than IBRS around firmware
/// (`bugs.c:2028-2036`), and has no `early_init_amd` to set `IBPB_BRTYPE`, so
/// safe RET lacks its microcode (`bugs.c:2672,2735-2736`).
#[test]
fn hygon_decides_as_linux_does() {
    let dhyana = |readable| Facts {
        vendor: Vendor::from_id(b"HygonGenuine"),
        signature: 0x0090_0f01,
        cpuid_8000_0008_ebx: CPUID_8000_0008_EBX_AMD_IBPB
            | CPUID_8000_0008_EBX_AMD_IBRS
            | CPUID_8000_0008_EBX_AMD_STIBP,
        ls_cfg_readable: Some(readable),
        ..ZEN1
    };
    let d = assert_capture(
        &dhyana(true),
        "/sys/devices/system/cpu/vulnerabilities/gather_data_sampling:Not affected
/sys/devices/system/cpu/vulnerabilities/indirect_target_selection:Not affected
/sys/devices/system/cpu/vulnerabilities/itlb_multihit:Not affected
/sys/devices/system/cpu/vulnerabilities/l1tf:Not affected
/sys/devices/system/cpu/vulnerabilities/mds:Not affected
/sys/devices/system/cpu/vulnerabilities/meltdown:Not affected
/sys/devices/system/cpu/vulnerabilities/mmio_stale_data:Not affected
/sys/devices/system/cpu/vulnerabilities/reg_file_data_sampling:Not affected
/sys/devices/system/cpu/vulnerabilities/retbleed:Mitigation: untrained return thunk; SMT enabled with STIBP protection
/sys/devices/system/cpu/vulnerabilities/spec_rstack_overflow:Vulnerable: Safe RET, no microcode
/sys/devices/system/cpu/vulnerabilities/spec_store_bypass:Mitigation: Speculative Store Bypass disabled via prctl
/sys/devices/system/cpu/vulnerabilities/spectre_v1:Mitigation: usercopy/swapgs barriers and __user pointer sanitization
/sys/devices/system/cpu/vulnerabilities/spectre_v2:Mitigation: Retpolines; IBPB: conditional; STIBP: always-on; RSB filling; PBRSB-eIBRS: Not affected; BHI: Not affected
/sys/devices/system/cpu/vulnerabilities/srbds:Not affected
/sys/devices/system/cpu/vulnerabilities/tsa:Not affected
/sys/devices/system/cpu/vulnerabilities/tsx_async_abort:Not affected
/sys/devices/system/cpu/vulnerabilities/vmscape:Mitigation: IBPB before exit to userspace",
    );
    let expected = State {
        spectre_v2: Some(SpectreV2::Retpoline),
        ibpb: true,
        stibp: Stibp::StrictPreferred,
        retbleed: Retbleed::Unret,
        ssb: Ssb::Prctl,
        vmscape: Vmscape::IbpbExitToUser,
        srso: Some(Srso::SafeRetUcodeNeeded),
        spec_ctrl: SPEC_CTRL_STIBP,
        ..NONE
    };
    assert_eq!(state(&d), expected);
    assert_eq!(state(&decide(&dhyana(false))), State { ssb: Ssb::None, ..expected });
}

/// Family 0x18 is below 0x19, so a Dhyana without SMT has SRSO ruled out by
/// `SRSO_NO` once `IBPB_BRTYPE` is set (`bugs.c:2689-2691`), shown as "SMT
/// disabled" (`bugs.c:3284-3285`).
#[test]
fn hygon_without_smt_and_with_ibpb_brtype_has_srso_smt_disabled() {
    let dhyana = Facts {
        vendor: Vendor::from_id(b"HygonGenuine"),
        signature: 0x0090_0f01,
        cpuid_8000_0008_ebx: CPUID_8000_0008_EBX_AMD_IBPB
            | CPUID_8000_0008_EBX_AMD_IBRS
            | CPUID_8000_0008_EBX_AMD_STIBP,
        cpuid_8000_0021_eax: CPUID_8000_0021_EAX_IBPB_BRTYPE,
        ls_cfg_readable: Some(true),
        smt: false,
        ..ZEN1
    };
    assert_eq!(decide(&dhyana).srso, Some(Srso::SmtDisabled));
    assert_eq!(line(&dhyana, Vuln::SpecRstackOverflow), "Mitigation: SMT disabled");
}

#[test]
#[should_panic(expected = "MSR_AMD64_LS_CFG is probed exactly where")]
fn a_hygon_cpu_without_its_ls_cfg_probe_is_a_contradiction() {
    decide(&Facts {
        vendor: Vendor::from_id(b"HygonGenuine"),
        signature: 0x0090_0f01,
        cpuid_8000_0008_ebx: 0,
        ..ZEN1
    });
}

/// A Zhaoxin, family 7 model 0x1B, with SMT: its whitelist row rules out
/// Spectre v2 but not v1 (`common.c:1242,1426`), so neither IBPB, STIBP nor IBRS
/// around firmware is set up (`bugs.c:1869-1871,1507-1508`); it reads
/// `IA32_FEAT_CTL` (`zhaoxin.c:97`). An unknown vendor's family 7 matches no
/// row and has Spectre v2.
#[test]
fn zhaoxin_without_spectre_v2_sets_up_no_ibpb() {
    let zhaoxin = Facts {
        vendor: Vendor::from_id(b"  Shanghai  "),
        signature: 0x0001_07b5,
        microcode: 0,
        cpuid_1_ecx: CPUID_1_ECX_VMX,
        cpuid_7_0_ebx: 0,
        cpuid_7_0_edx: CPUID_7_0_EDX_SPEC_CTRL
            | CPUID_7_0_EDX_INTEL_STIBP
            | CPUID_7_0_EDX_SPEC_CTRL_SSBD,
        cpuid_7_2_edx: 0,
        cpuid_8000_0008_ebx: 0,
        cpuid_8000_0021_eax: 0,
        cpuid_8000_0021_ecx: 0,
        arch_capabilities: 0,
        mcu_opt_ctrl: None,
        feat_ctl: Some(0),
        ls_cfg_readable: None,
        sbpb_write_accepted: None,
        smt: true,
    };
    let d = decide(&zhaoxin);
    assert_eq!(
        state(&d),
        State { ssb: Ssb::Prctl, mds: Some(Mds::Vmwerv), clear_cpu_buf: true, ..NONE }
    );
    assert_eq!(line(&zhaoxin, Vuln::SpectreV2), "Not affected");
    assert_eq!(
        line(&zhaoxin, Vuln::SpectreV1),
        "Mitigation: usercopy/swapgs barriers and __user pointer sanitization"
    );
    assert_eq!(
        line(&zhaoxin, Vuln::Mds),
        "Vulnerable: Clear CPU buffers attempted, no microcode; SMT vulnerable"
    );
    assert_eq!(line(&zhaoxin, Vuln::ItlbMultihit), "KVM: Mitigation: VMX disabled");
    let unknown = Facts { vendor: Vendor::from_id(b"GenuineIotel"), feat_ctl: None, ..zhaoxin };
    assert_eq!(decide(&unknown).spectre_v2, Some(SpectreV2::Retpoline));
}

/// A Centaur family 5 is `NO_SPECULATION` (`common.c:1184`) and reads
/// `IA32_FEAT_CTL` (`centaur.c:217`); ITLB multihit, set before that return
/// (`common.c:1419-1424`), is all it has.
#[test]
fn centaur_family_5_has_only_itlb_multihit() {
    let centaur = Facts {
        vendor: Vendor::from_id(b"CentaurHauls"),
        signature: 0x0000_0540,
        feat_ctl: Some(0),
        cpuid_1_ecx: CPUID_1_ECX_VMX,
        ..TCG
    };
    assert_eq!(state(&decide(&centaur)), NONE);
    assert_eq!(line(&centaur, Vuln::ItlbMultihit), "KVM: Mitigation: VMX disabled");
    assert_eq!(line(&centaur, Vuln::SpectreV1), "Not affected");
}

#[test]
#[should_panic(expected = "PRED_CMD.SBPB is probed exactly where")]
fn a_native_zen3_without_its_probe_is_a_contradiction() {
    decide(&Facts { cpuid_1_ecx: 0, ..MILAN_GUEST });
}

/// `x86_family` adds the extended family only to family 0xF, and `x86_model`
/// the extended model only from family 6 (`arch/x86/lib/cpu.c:6-37`).
#[test]
fn family_and_model_are_derived_as_arch_x86_lib_derives_them() {
    let at = |sig| {
        let id = Ident::new(Vendor::Intel, sig);
        (id.family, id.model, id.stepping)
    };
    assert_eq!(at(0x00a0_0f11), (0x19, 0x01, 1));
    assert_eq!(at(0x0ff0_06c1), (6, 0x0c, 1));
    assert_eq!(at(0x000f_0521), (5, 0x02, 1));
}

/// Each `*_NO` bit and feature test in `cpu_set_bug_bits` rules its bug out on
/// [`T14`]: `SSB_NO` (`common.c:1431-1434`), `PBRSB_NO` (`common.c:1440-1445`),
/// `GDS_NO` and a missing AVX (`common.c:1521-1523`), `ITS_NO` and `BHI_CTRL`
/// (`common.c:1391-1398`); `RSBA` gives RETBleed (`common.c:1502-1505`), which
/// eIBRS mitigates (`bugs.c:1180-1183`, `retbleed_strings` 1003).
#[test]
fn the_t14s_no_bits_each_rule_out_their_bug() {
    let t14 = state(&decide(&T14));
    let arch = |bit| Facts { arch_capabilities: T14.arch_capabilities | bit, ..T14 };
    assert_eq!(line(&arch(ARCH_CAP_SSB_NO), Vuln::SpecStoreBypass), "Not affected");
    assert_eq!(state(&decide(&arch(ARCH_CAP_SSB_NO))), State { ssb: Ssb::None, ..t14 });
    assert_eq!(
        line(&arch(ARCH_CAP_PBRSB_NO), Vuln::SpectreV2),
        "Mitigation: Enhanced / Automatic IBRS; IBPB: conditional; PBRSB-eIBRS: Not affected; \
         BHI: SW loop, KVM: SW loop"
    );
    assert_eq!(line(&arch(ARCH_CAP_GDS_NO), Vuln::GatherDataSampling), "Not affected");
    let no_avx = Facts { cpuid_1_ecx: T14.cpuid_1_ecx & !CPUID_1_ECX_AVX, ..T14 };
    assert_eq!(line(&no_avx, Vuln::GatherDataSampling), "Not affected");
    assert_eq!(line(&arch(ARCH_CAP_ITS_NO), Vuln::IndirectTargetSelection), "Not affected");
    let rsba = arch(ARCH_CAP_RSBA);
    assert_eq!(state(&decide(&rsba)), State { retbleed: Retbleed::Eibrs, ..t14 });
    assert_eq!(line(&rsba, Vuln::Retbleed), "Mitigation: Enhanced IBRS");
    // `BHI_CTRL` also puts BHI in hardware (`bugs.c:1847-1849`).
    let bhi_ctrl = Facts { cpuid_7_2_edx: T14.cpuid_7_2_edx | CPUID_7_2_EDX_BHI_CTRL, ..T14 };
    assert_eq!(
        state(&decide(&bhi_ctrl)),
        State {
            bhi: Some(Bhi::BhiDisS),
            its: false,
            spec_ctrl: SPEC_CTRL_IBRS | SPEC_CTRL_BHI_DIS_S,
            ..t14
        }
    );
}

/// RFDS by `RFDS_CLEAR` where no row names it, whose `VERW` clears CPU buffers
/// (`common.c:1370-1386`; `bugs.c:529-532`, `rfds_strings` 516), and by
/// Alder Lake's row without `RFDS_NO` or `RFDS_CLEAR`, whose microcode is
/// missing (`common.c:1316`; `bugs.c:531-532`, `rfds_strings` 517).
#[test]
fn rfds_is_cleared_only_with_rfds_clear() {
    let clear = Facts {
        arch_capabilities: (T14.arch_capabilities & !ARCH_CAP_RFDS_NO) | ARCH_CAP_RFDS_CLEAR,
        ..T14
    };
    assert_eq!(
        state(&decide(&clear)),
        State { rfds: Some(Rfds::Verw), clear_cpu_buf: true, ..state(&decide(&T14)) }
    );
    assert_eq!(line(&clear, Vuln::RegFileDataSampling), "Mitigation: Clear Register File");
    let adl = Facts {
        signature: 0x0009_0672,
        arch_capabilities: T14.arch_capabilities & !ARCH_CAP_RFDS_NO,
        ..T14
    };
    assert_eq!(decide(&adl).rfds, Some(Rfds::UcodeNeeded));
    assert_eq!(line(&adl, Vuln::RegFileDataSampling), "Vulnerable: No microcode");
    assert!(!decide(&adl).clear_cpu_buf);
}

/// [`COMETLAKE`] without `ARCH_CAPABILITIES` bits, varied one fact at a time:
/// `TAA_NO` rules TAA out (`common.c:1467-1470`); without `MD_CLEAR` MDS is
/// `VMWERV` (`bugs.c:274-275`) and TAA's and MMIO's microcode is missing
/// (`bugs.c:353-356,468-474`); without `FLUSH_L1D` MMIO's is (`bugs.c:468-474`);
/// without `SRBDS_CTRL` SRBDS's is (`bugs.c:693-694`, `srbds_strings` 633);
/// either RDRAND or RDSEED gives SRBDS (`common.c:1480-1483`).
#[test]
fn comet_lake_varies_one_fact_at_a_time() {
    let all = State {
        mds: Some(Mds::Full),
        taa: Some(Taa::Verw),
        mmio: Some(Mmio::Verw),
        srbds: Some(Srbds::Full),
        clear_cpu_buf: true,
        ..COMETLAKE_STATE
    };
    let taa_no = Facts { arch_capabilities: ARCH_CAP_TAA_NO, ..COMETLAKE };
    assert_eq!(state(&decide(&taa_no)), State { taa: None, ..all });
    assert_eq!(line(&taa_no, Vuln::TsxAsyncAbort), "Not affected");
    let edx7 = |clear: u32| Facts { cpuid_7_0_edx: COMETLAKE.cpuid_7_0_edx & !clear, ..COMETLAKE };
    assert_eq!(
        state(&decide(&edx7(CPUID_7_0_EDX_MD_CLEAR))),
        State {
            mds: Some(Mds::Vmwerv),
            taa: Some(Taa::UcodeNeeded),
            mmio: Some(Mmio::UcodeNeeded),
            ..all
        }
    );
    assert_eq!(
        state(&decide(&edx7(CPUID_7_0_EDX_FLUSH_L1D))),
        State { mmio: Some(Mmio::UcodeNeeded), ..all }
    );
    let no_srbds_ctrl = edx7(CPUID_7_0_EDX_SRBDS_CTRL);
    assert_eq!(state(&decide(&no_srbds_ctrl)), State { srbds: Some(Srbds::UcodeNeeded), ..all });
    assert_eq!(line(&no_srbds_ctrl, Vuln::Srbds), "Vulnerable: No microcode");
    let rdseed = Facts { cpuid_1_ecx: COMETLAKE.cpuid_1_ecx & !CPUID_1_ECX_RDRAND, ..COMETLAKE };
    assert_eq!(decide(&rdseed).srbds, Some(Srbds::Full));
    let rdrand = Facts { cpuid_7_0_ebx: COMETLAKE.cpuid_7_0_ebx & !CPUID_7_0_EBX_RDSEED, ..COMETLAKE };
    assert_eq!(decide(&rdrand).srbds, Some(Srbds::Full));
    let neither = Facts { cpuid_7_0_ebx: rdrand.cpuid_7_0_ebx, ..rdseed };
    assert_eq!(decide(&neither).srbds, None);
    assert_eq!(line(&neither, Vuln::Srbds), "Not affected");
}

/// [`COMETLAKE`] as a guest: BHI from the hypervisor bit (`common.c:1533-1536`)
/// takes the SW loop under IBRS (`bugs.c:1851-1857`), VMSCAPE is bare-metal
/// only (`common.c:1545-1548`), and TAA's host SMT state is unknown
/// (`bugs.c:3138-3141`).
#[test]
fn a_comet_lake_guest_cannot_know_its_hosts_smt() {
    let guest = Facts { cpuid_1_ecx: COMETLAKE.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR, ..COMETLAKE };
    assert_eq!(
        state(&decide(&guest)),
        State {
            bhi: Some(Bhi::SwLoop),
            mds: Some(Mds::Full),
            taa: Some(Taa::Verw),
            mmio: Some(Mmio::Verw),
            srbds: Some(Srbds::Hypervisor),
            gds: Some(Gds::Hypervisor),
            vmscape: Vmscape::None,
            clear_cpu_buf: true,
            ..COMETLAKE_STATE
        }
    );
    assert_eq!(
        line(&guest, Vuln::TsxAsyncAbort),
        "Mitigation: Clear CPU buffers; SMT Host state unknown"
    );
}

/// TAA without RTM in CPUID comes from `TSX_CTRL_MSR` (`common.c:1467-1470`)
/// and is "TSX disabled" (`bugs.c:335-338`). Without RTM and without `TSX_CTRL`
/// there is no TAA, and SRBDS is "TSX disabled" on an `MDS_NO` CPU without MMIO
/// (`bugs.c:688-690`) and the microcode's otherwise (`bugs.c:693-695`).
#[test]
fn rtm_absent_from_cpuid_is_rtm_off() {
    let no_rtm = |arch| Facts {
        cpuid_7_0_ebx: COMETLAKE.cpuid_7_0_ebx & !CPUID_7_0_EBX_RTM,
        arch_capabilities: arch,
        ..COMETLAKE
    };
    assert_eq!(
        state(&decide(&no_rtm(ARCH_CAP_MDS_NO | ARCH_CAP_TSX_CTRL_MSR))),
        State {
            tsx: Tsx::Disable,
            taa: Some(Taa::TsxDisabled),
            mmio: Some(Mmio::UcodeNeeded),
            srbds: Some(Srbds::Full),
            ..COMETLAKE_STATE
        }
    );
    let immune = ARCH_CAP_FBSDP_NO | ARCH_CAP_PSDP_NO | ARCH_CAP_SBDR_SSDP_NO;
    assert_eq!(
        state(&decide(&no_rtm(ARCH_CAP_MDS_NO | immune))),
        State { srbds: Some(Srbds::TsxOff), ..COMETLAKE_STATE }
    );
    assert_eq!(
        state(&decide(&no_rtm(immune))),
        State {
            mds: Some(Mds::Full),
            srbds: Some(Srbds::Full),
            clear_cpu_buf: true,
            ..COMETLAKE_STATE
        }
    );
}

/// The bad-microcode rows key model and stepping together (`intel.c:177-178`):
/// KABYLAKE stepping 8 has no row, and Comet Lake stepping 0xA is not
/// KABYLAKE's stepping 0xA.
#[test]
fn a_bad_microcode_row_is_its_model_and_its_stepping() {
    for signature in [0x0009_06e8, 0x000a_065a] {
        let d = decide(&Facts { signature, microcode: 0x80, ..KABYLAKE });
        assert_eq!((d.ibpb, d.ssb), (true, Ssb::Prctl), "{signature:#x}");
    }
}

/// An unknown vendor's family 6 matches no row (`match.c:36-57`): `RSBA` gives
/// RETBleed (`common.c:1502-1505`), which neither the AMD nor the Intel
/// selection takes up (`bugs.c:1101-1106,1175`), and IBRS guards firmware
/// (`bugs.c:2038-2041`). Its MMIO is unknown (`common.c:1498-1499`).
#[test]
fn an_unknown_vendors_retbleed_is_unmitigated() {
    let unknown = Facts {
        vendor: Vendor::from_id(b"GenuineIotel"),
        signature: 0x0009_06ea,
        microcode: 0,
        cpuid_1_ecx: 0,
        cpuid_7_0_ebx: 0,
        cpuid_7_0_edx: CPUID_7_0_EDX_SPEC_CTRL | CPUID_7_0_EDX_ARCH_CAPABILITIES,
        cpuid_7_2_edx: 0,
        cpuid_8000_0008_ebx: 0,
        cpuid_8000_0021_eax: 0,
        cpuid_8000_0021_ecx: 0,
        arch_capabilities: ARCH_CAP_RSBA,
        mcu_opt_ctrl: None,
        feat_ctl: None,
        ls_cfg_readable: None,
        sbpb_write_accepted: None,
        smt: false,
    };
    assert_eq!(
        state(&decide(&unknown)),
        State {
            spectre_v2: Some(SpectreV2::Retpoline),
            ibpb: true,
            ibrs_fw: true,
            mds: Some(Mds::Vmwerv),
            clear_cpu_buf: true,
            ..NONE
        }
    );
    assert_eq!(line(&unknown, Vuln::Retbleed), "Vulnerable");
    assert_eq!(line(&unknown, Vuln::MmioStaleData), "Unknown: No mitigations");
}

/// [`ZEN1`] with `BTC_NO` has no RETBleed (`common.c:1502-1505`), so IBRS, not
/// IBPB, guards firmware (`bugs.c:2028-2041`); with `AMD_SSB_NO` it has no SSB
/// (`common.c:1431-1434`). Without AMD_IBPB there is no `IBPB_BRTYPE`
/// (`amd.c:800-801`), so safe RET lacks its microcode (`bugs.c:2735-2736`),
/// VMSCAPE has no IBPB (`bugs.c:2851-2855`), and IBRS guards firmware.
#[test]
fn zen1_by_btc_no_ssb_no_and_ibpb() {
    let ebx8 = |set, clear: u32| Facts {
        cpuid_8000_0008_ebx: (ZEN1.cpuid_8000_0008_ebx | set) & !clear,
        ..ZEN1
    };
    let expected = State {
        spectre_v2: Some(SpectreV2::Retpoline),
        ibpb: true,
        ibrs_fw: true,
        ssb: Ssb::Prctl,
        vmscape: Vmscape::IbpbExitToUser,
        srso: Some(Srso::SafeRet),
        ..NONE
    };
    let btc_no = ebx8(CPUID_8000_0008_EBX_BTC_NO, 0);
    assert_eq!(state(&decide(&btc_no)), expected);
    assert_eq!(line(&btc_no, Vuln::Retbleed), "Not affected");
    assert_eq!(line(&ebx8(CPUID_8000_0008_EBX_AMD_SSB_NO, 0), Vuln::SpecStoreBypass), "Not affected");
    let no_ibpb = ebx8(0, CPUID_8000_0008_EBX_AMD_IBPB);
    assert_eq!(
        state(&decide(&no_ibpb)),
        State {
            ibpb: false,
            retbleed: Retbleed::Unret,
            vmscape: Vmscape::None,
            srso: Some(Srso::SafeRetUcodeNeeded),
            ..expected
        }
    );
    assert_eq!(
        line(&no_ibpb, Vuln::SpectreV2),
        "Mitigation: Retpolines; IBRS_FW; STIBP: disabled; RSB filling; PBRSB-eIBRS: Not affected; \
         BHI: Not affected"
    );
    assert_eq!(line(&no_ibpb, Vuln::Vmscape), "Vulnerable");
}

/// A Rome (Zen2) guest: `early_init_amd` sets no `IBPB_BRTYPE` under a
/// hypervisor (`amd.c:799`), so SRSO is safe RET without its microcode even
/// without SMT (`bugs.c:2687-2703,2735-2736`); `VIRT_SSBD` alone gives SSBD
/// (`common.c:990-992`) and leaves `LS_CFG` unprobed (`amd.c:576-578`).
#[test]
fn a_zen2_guest_has_no_ibpb_brtype() {
    let rome = Facts {
        signature: 0x0083_0f10,
        cpuid_8000_0021_eax: 0,
        ..MILAN_GUEST
    };
    assert_eq!(
        state(&decide(&rome)),
        State {
            spectre_v2: Some(SpectreV2::Retpoline),
            ibpb: true,
            retbleed: Retbleed::Unret,
            ssb: Ssb::Prctl,
            srso: Some(Srso::SafeRetUcodeNeeded),
            ..NONE
        }
    );
    assert_eq!(line(&rome, Vuln::SpecRstackOverflow), "Vulnerable: Safe RET, no microcode");
    let virt_ssbd = Facts {
        cpuid_8000_0008_ebx: (rome.cpuid_8000_0008_ebx & !CPUID_8000_0008_EBX_AMD_SSBD)
            | CPUID_8000_0008_EBX_VIRT_SSBD,
        ..rome
    };
    assert_eq!(decide(&virt_ssbd).ssb, Ssb::Prctl);
}

/// A native family 0x19 with `SRSO_NO` keeps SBPB (`bugs.c:2674-2678`); with
/// `IBPB_BRTYPE` enumerated it is not probed (`amd.c:799`) and has safe RET's
/// microcode (`bugs.c:2672,2733-2734`).
#[test]
fn zen3_by_srso_no_and_an_enumerated_ibpb_brtype() {
    let srso_no = Facts {
        cpuid_8000_0021_eax: MILAN_GUEST.cpuid_8000_0021_eax | CPUID_8000_0021_EAX_SRSO_NO,
        ..native_milan(0x0a00_11d7, true)
    };
    assert_eq!(
        state(&decide(&srso_no)),
        State { sbpb: true, tsa: Some(Tsa::Full), clear_cpu_buf: true, ..NATIVE_MILAN }
    );
    assert_eq!(line(&srso_no, Vuln::SpecRstackOverflow), "Not affected");
    let brtype = Facts {
        cpuid_8000_0021_eax: MILAN_GUEST.cpuid_8000_0021_eax | CPUID_8000_0021_EAX_IBPB_BRTYPE,
        sbpb_write_accepted: None,
        ..native_milan(0x0a00_11d7, false)
    };
    assert_eq!(
        state(&decide(&brtype)),
        State { srso: Some(Srso::SafeRet), tsa: Some(Tsa::Full), clear_cpu_buf: true, ..NATIVE_MILAN }
    );
}

/// A native Turin with SMT: AutoIBRS leaves STIBP selectable
/// (`bugs.c:1564-1567`), and AMD_STIBP_ALWAYS_ON makes it strict
/// (`bugs.c:1575-1577`); `stibp_state` prints it (`bugs.c:3176-3186`).
#[test]
fn autoibrs_leaves_stibp_to_select() {
    let turin = Facts {
        cpuid_1_ecx: TURIN_GUEST.cpuid_1_ecx & !CPUID_1_ECX_HYPERVISOR,
        smt: true,
        ..TURIN_GUEST
    };
    assert_eq!(
        state(&decide(&turin)),
        State {
            spectre_v2: Some(SpectreV2::Eibrs),
            ibpb: true,
            stibp: Stibp::StrictPreferred,
            ssb: Ssb::Prctl,
            sbpb: true,
            efer_autoibrs: true,
            spec_ctrl: SPEC_CTRL_STIBP,
            ..NONE
        }
    );
    assert_eq!(
        line(&turin, Vuln::SpectreV2),
        "Mitigation: Enhanced / Automatic IBRS; IBPB: conditional; STIBP: always-on; \
         PBRSB-eIBRS: Not affected; BHI: Not affected"
    );
}

/// `VERW_CLEAR` enumerated to a guest is TSA's microcode (`bugs.c:2928-2929`,
/// `tsa_strings` 2892).
#[test]
fn a_guest_with_verw_clear_mitigates_tsa() {
    let guest = Facts {
        cpuid_8000_0021_eax: MILAN_GUEST.cpuid_8000_0021_eax | CPUID_8000_0021_EAX_VERW_CLEAR,
        ..MILAN_GUEST
    };
    assert_eq!(decide(&guest).tsa, Some(Tsa::Full));
    assert_eq!(line(&guest, Vuln::Tsa), "Mitigation: Clear CPU buffers");
}

/// Without `RRSBA` there is nothing to disable (`bugs.c:1728-1731`), so a
/// retpoline guest leaves BHI to retpolines without `RRSBA_DIS_S`.
#[test]
fn a_retpoline_guest_without_rrsba_leaves_bhi_to_retpolines() {
    let guest = Facts {
        cpuid_1_ecx: T14.cpuid_1_ecx | CPUID_1_ECX_HYPERVISOR,
        arch_capabilities: T14.arch_capabilities & !ARCH_CAP_IBRS_ALL,
        ..T14
    };
    let d = decide(&guest);
    assert_eq!((d.bhi, d.spec_ctrl), (Some(Bhi::Retpoline), 0));
}

/// The two probes are AMD's and Hygon's: an Intel family 0x17 or 0x19 is
/// probed for neither.
#[test]
fn only_amd_and_hygon_are_probed() {
    for signature in [0x0080_0f00, 0x00a0_0f00] {
        let d = decide(&Facts { signature, ..TCG_INTEL });
        assert_eq!(d.spectre_v2, Some(SpectreV2::Retpoline), "{signature:#x}");
    }
}

/// [`TCG`]'s facts under Intel's name.
const TCG_INTEL: Facts = Facts { vendor: Vendor::from_id(b"GenuineIntel"), ..TCG };
