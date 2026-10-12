//! Linux's five CPU tables, row for row: `cpu_vuln_whitelist` and
//! `cpu_vuln_blacklist` (`common.c:1182-1344`), `spectre_bad_microcodes`
//! (`intel.c:141-163`), `amd_check_tsa_microcode`'s (`amd.c:472-515`) and
//! `test_intel`'s (`arch/x86/events/msr.c:40-122`). A
//! lookup in the first two is `x86_match_cpu` (`match.c:36-57`): the
//! first row whose vendor, family, model and stepping all match, `0` matching
//! any.
//!
//! The whitelist's `NSC` and `VORTEX` rows are left out: x86-64 Linux builds
//! neither vendor (`CONFIG_CPU_SUP_CYRIX_32`, `CONFIG_CPU_SUP_VORTEX_32`), so
//! no CPU it identifies matches them.

use crate::{Ident, Vendor};

/// A whitelist row's flags (`common.c:1157-1168`).
#[derive(Clone, Copy)]
pub(crate) struct Wl(u16);
/// A blacklist row's flags (`common.c:1260-1282`).
#[derive(Clone, Copy)]
pub(crate) struct Bl(u16);

impl core::ops::BitOr for Bl {
    type Output = Self;
    fn bitor(self, o: Self) -> Self {
        Self(self.0 | o.0)
    }
}

pub(crate) const NO_SPECULATION: Wl = Wl(1 << 0);
pub(crate) const NO_MELTDOWN: Wl = Wl(1 << 1);
pub(crate) const NO_SSB: Wl = Wl(1 << 2);
pub(crate) const NO_L1TF: Wl = Wl(1 << 3);
pub(crate) const NO_MDS: Wl = Wl(1 << 4);
pub(crate) const MSBDS_ONLY: Wl = Wl(1 << 5);
pub(crate) const NO_SWAPGS: Wl = Wl(1 << 6);
pub(crate) const NO_ITLB_MULTIHIT: Wl = Wl(1 << 7);
pub(crate) const NO_SPECTRE_V2: Wl = Wl(1 << 8);
pub(crate) const NO_MMIO: Wl = Wl(1 << 9);
pub(crate) const NO_EIBRS_PBRSB: Wl = Wl(1 << 10);
pub(crate) const NO_BHI: Wl = Wl(1 << 11);

pub(crate) const SRBDS: Bl = Bl(1 << 0);
pub(crate) const MMIO: Bl = Bl(1 << 1);
pub(crate) const MMIO_SBDS: Bl = Bl(1 << 2);
pub(crate) const RETBLEED: Bl = Bl(1 << 3);
pub(crate) const SMT_RSB: Bl = Bl(1 << 4);
pub(crate) const SRSO: Bl = Bl(1 << 5);
pub(crate) const GDS: Bl = Bl(1 << 6);
pub(crate) const RFDS: Bl = Bl(1 << 7);
pub(crate) const ITS: Bl = Bl(1 << 8);
pub(crate) const ITS_NATIVE_ONLY: Bl = Bl(1 << 9);
pub(crate) const TSA: Bl = Bl(1 << 10);
pub(crate) const VMSCAPE: Bl = Bl(1 << 11);

macro_rules! wl { ($($f:ident)|+) => { Wl(0 $(| $f.0)+) }; }
macro_rules! bl { ($($f:ident)|+) => { Bl(0 $(| $f.0)+) }; }

/// `X86_FAMILY_ANY`, `X86_MODEL_ANY` and `X86_STEPPING_ANY`
/// (`include/linux/mod_devicetable.h:700-702`).
const ANY: u32 = 0;

/// `X86_STEPPINGS(mins, maxs)`, `GENMASK(maxs, mins)` (`asm/cpu_device_id.h:59`).
const fn steppings(min: u32, max: u32) -> u32 {
    ((1 << (max + 1)) - 1) & !((1 << min) - 1)
}

pub(crate) struct Row<F> {
    vendor: Option<Vendor>,
    family: u32,
    model: u32,
    steppings: u32,
    flags: F,
}

const fn row<F>(vendor: Option<Vendor>, family: u32, model: u32, flags: F) -> Row<F> {
    Row { vendor, family, model, steppings: ANY, flags }
}
const fn intel<F>(model: u32, flags: F) -> Row<F> {
    row(Some(Vendor::Intel), 6, model, flags)
}
const fn intel_steppings(model: u32, steppings: u32, flags: Bl) -> Row<Bl> {
    Row { vendor: Some(Vendor::Intel), family: 6, model, steppings, flags }
}
const fn amd<F>(family: u32, flags: F) -> Row<F> {
    row(Some(Vendor::Amd), family, ANY, flags)
}
const fn hygon<F>(family: u32, flags: F) -> Row<F> {
    row(Some(Vendor::Hygon), family, ANY, flags)
}

fn lookup<F: Copy>(rows: &[Row<F>], id: &Ident) -> Option<F> {
    rows.iter()
        .find(|r| {
            r.vendor.is_none_or(|v| v == id.vendor)
                && (r.family == ANY || r.family == id.family)
                && (r.model == ANY || r.model == id.model)
                && (r.steppings == ANY || r.steppings & (1 << id.stepping) != 0)
        })
        .map(|r| r.flags)
}

/// `cpu_matches(cpu_vuln_whitelist, which)` (`common.c:1346-1351`).
pub(crate) fn whitelisted(id: &Ident, which: Wl) -> bool {
    lookup(WHITELIST, id).is_some_and(|f| f.0 & which.0 != 0)
}

/// `cpu_matches(cpu_vuln_blacklist, which)` (`common.c:1346-1351`).
pub(crate) fn blacklisted(id: &Ident, which: Bl) -> bool {
    lookup(BLACKLIST, id).is_some_and(|f| f.0 & which.0 != 0)
}

// `arch/x86/include/asm/intel-family.h`, family 6, the models the tables name.
const CORE_YONAH: u32 = 0x0E;
const NEHALEM: u32 = 0x1E;
const NEHALEM_G: u32 = 0x1F;
const NEHALEM_EP: u32 = 0x1A;
const NEHALEM_EX: u32 = 0x2E;
const WESTMERE: u32 = 0x25;
const WESTMERE_EP: u32 = 0x2C;
const WESTMERE_EX: u32 = 0x2F;
const SANDYBRIDGE: u32 = 0x2A;
const SANDYBRIDGE_X: u32 = 0x2D;
const IVYBRIDGE: u32 = 0x3A;
const IVYBRIDGE_X: u32 = 0x3E;
const HASWELL: u32 = 0x3C;
const HASWELL_X: u32 = 0x3F;
const HASWELL_L: u32 = 0x45;
const HASWELL_G: u32 = 0x46;
const BROADWELL: u32 = 0x3D;
const BROADWELL_G: u32 = 0x47;
const BROADWELL_X: u32 = 0x4F;
const BROADWELL_D: u32 = 0x56;
const SKYLAKE_L: u32 = 0x4E;
const SKYLAKE: u32 = 0x5E;
const SKYLAKE_X: u32 = 0x55;
const KABYLAKE_L: u32 = 0x8E;
const KABYLAKE: u32 = 0x9E;
const COMETLAKE: u32 = 0xA5;
const COMETLAKE_L: u32 = 0xA6;
const CANNONLAKE_L: u32 = 0x66;
const ICELAKE_X: u32 = 0x6A;
const ICELAKE_D: u32 = 0x6C;
const ICELAKE_L: u32 = 0x7E;
const ICELAKE: u32 = 0x7D;
const ROCKETLAKE: u32 = 0xA7;
const TIGERLAKE_L: u32 = 0x8C;
const TIGERLAKE: u32 = 0x8D;
const SAPPHIRERAPIDS_X: u32 = 0x8F;
const EMERALDRAPIDS_X: u32 = 0xCF;
const GRANITERAPIDS_X: u32 = 0xAD;
const GRANITERAPIDS_D: u32 = 0xAE;
const LAKEFIELD: u32 = 0x8A;
const ALDERLAKE: u32 = 0x97;
const ALDERLAKE_L: u32 = 0x9A;
const RAPTORLAKE: u32 = 0xB7;
const RAPTORLAKE_P: u32 = 0xBA;
const RAPTORLAKE_S: u32 = 0xBF;
const METEORLAKE_L: u32 = 0xAA;
const METEORLAKE: u32 = 0xAC;
const ARROWLAKE_H: u32 = 0xC5;
const ARROWLAKE: u32 = 0xC6;
const ARROWLAKE_U: u32 = 0xB5;
const LUNARLAKE_M: u32 = 0xBD;
const ATOM_BONNELL: u32 = 0x1C;
const ATOM_BONNELL_MID: u32 = 0x26;
const ATOM_SALTWELL: u32 = 0x36;
const ATOM_SALTWELL_MID: u32 = 0x27;
const ATOM_SALTWELL_TABLET: u32 = 0x35;
const ATOM_SILVERMONT: u32 = 0x37;
const ATOM_SILVERMONT_D: u32 = 0x4D;
const ATOM_SILVERMONT_MID: u32 = 0x4A;
const ATOM_AIRMONT: u32 = 0x4C;
const ATOM_AIRMONT_MID: u32 = 0x5A;
const ATOM_AIRMONT_NP: u32 = 0x75;
const ATOM_GOLDMONT: u32 = 0x5C;
const ATOM_GOLDMONT_D: u32 = 0x5F;
const ATOM_GOLDMONT_PLUS: u32 = 0x7A;
const ATOM_TREMONT_D: u32 = 0x86;
const ATOM_TREMONT: u32 = 0x96;
const ATOM_TREMONT_L: u32 = 0x9C;
const ATOM_GRACEMONT: u32 = 0xBE;
const ATOM_CRESTMONT_X: u32 = 0xAF;
const XEON_PHI_KNL: u32 = 0x57;
const XEON_PHI_KNM: u32 = 0x85;

/// `cpu_vuln_whitelist` (`common.c:1182-1244`).
const WHITELIST: &[Row<Wl>] = &[
    row(None, 4, ANY, wl!(NO_SPECULATION)),
    row(Some(Vendor::Centaur), 5, ANY, wl!(NO_SPECULATION)),
    row(Some(Vendor::Intel), 5, ANY, wl!(NO_SPECULATION)),

    intel(TIGERLAKE, wl!(NO_MMIO)),
    intel(TIGERLAKE_L, wl!(NO_MMIO)),
    intel(ALDERLAKE, wl!(NO_MMIO)),
    intel(ALDERLAKE_L, wl!(NO_MMIO)),

    intel(ATOM_SALTWELL, wl!(NO_SPECULATION | NO_ITLB_MULTIHIT)),
    intel(ATOM_SALTWELL_TABLET, wl!(NO_SPECULATION | NO_ITLB_MULTIHIT)),
    intel(ATOM_SALTWELL_MID, wl!(NO_SPECULATION | NO_ITLB_MULTIHIT)),
    intel(ATOM_BONNELL, wl!(NO_SPECULATION | NO_ITLB_MULTIHIT)),
    intel(ATOM_BONNELL_MID, wl!(NO_SPECULATION | NO_ITLB_MULTIHIT)),

    intel(ATOM_SILVERMONT, wl!(NO_SSB | NO_L1TF | MSBDS_ONLY | NO_SWAPGS | NO_ITLB_MULTIHIT)),
    intel(ATOM_SILVERMONT_D, wl!(NO_SSB | NO_L1TF | MSBDS_ONLY | NO_SWAPGS | NO_ITLB_MULTIHIT)),
    intel(ATOM_SILVERMONT_MID, wl!(NO_SSB | NO_L1TF | MSBDS_ONLY | NO_SWAPGS | NO_ITLB_MULTIHIT)),
    intel(ATOM_AIRMONT, wl!(NO_SSB | NO_L1TF | MSBDS_ONLY | NO_SWAPGS | NO_ITLB_MULTIHIT)),
    intel(XEON_PHI_KNL, wl!(NO_SSB | NO_L1TF | MSBDS_ONLY | NO_SWAPGS | NO_ITLB_MULTIHIT)),
    intel(XEON_PHI_KNM, wl!(NO_SSB | NO_L1TF | MSBDS_ONLY | NO_SWAPGS | NO_ITLB_MULTIHIT)),

    intel(CORE_YONAH, wl!(NO_SSB)),

    intel(ATOM_AIRMONT_MID, wl!(NO_SSB | NO_L1TF | NO_SWAPGS | NO_ITLB_MULTIHIT | MSBDS_ONLY)),
    intel(ATOM_AIRMONT_NP, wl!(NO_SSB | NO_L1TF | NO_SWAPGS | NO_ITLB_MULTIHIT)),

    intel(ATOM_GOLDMONT, wl!(NO_MDS | NO_L1TF | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO)),
    intel(ATOM_GOLDMONT_D, wl!(NO_MDS | NO_L1TF | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO)),
    intel(ATOM_GOLDMONT_PLUS, wl!(NO_MDS | NO_L1TF | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_EIBRS_PBRSB)),

    intel(ATOM_TREMONT, wl!(NO_EIBRS_PBRSB)),
    intel(ATOM_TREMONT_L, wl!(NO_EIBRS_PBRSB)),
    intel(ATOM_TREMONT_D, wl!(NO_ITLB_MULTIHIT | NO_EIBRS_PBRSB)),

    amd(0x0f, wl!(NO_MELTDOWN | NO_SSB | NO_L1TF | NO_MDS | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_BHI)),
    amd(0x10, wl!(NO_MELTDOWN | NO_SSB | NO_L1TF | NO_MDS | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_BHI)),
    amd(0x11, wl!(NO_MELTDOWN | NO_SSB | NO_L1TF | NO_MDS | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_BHI)),
    amd(0x12, wl!(NO_MELTDOWN | NO_SSB | NO_L1TF | NO_MDS | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_BHI)),

    amd(ANY, wl!(NO_MELTDOWN | NO_L1TF | NO_MDS | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_EIBRS_PBRSB | NO_BHI)),
    hygon(ANY, wl!(NO_MELTDOWN | NO_L1TF | NO_MDS | NO_SWAPGS | NO_ITLB_MULTIHIT | NO_MMIO | NO_EIBRS_PBRSB | NO_BHI)),

    row(Some(Vendor::Centaur), 7, ANY, wl!(NO_SPECTRE_V2 | NO_SWAPGS | NO_MMIO | NO_BHI)),
    row(Some(Vendor::Zhaoxin), 7, ANY, wl!(NO_SPECTRE_V2 | NO_SWAPGS | NO_MMIO | NO_BHI)),
];

/// `cpu_vuln_blacklist` (`common.c:1284-1344`).
const BLACKLIST: &[Row<Bl>] = &[
    intel_steppings(SANDYBRIDGE_X, ANY, bl!(VMSCAPE)),
    intel_steppings(SANDYBRIDGE, ANY, bl!(VMSCAPE)),
    intel_steppings(IVYBRIDGE_X, ANY, bl!(VMSCAPE)),
    intel_steppings(IVYBRIDGE, ANY, bl!(SRBDS | VMSCAPE)),
    intel_steppings(HASWELL, ANY, bl!(SRBDS | VMSCAPE)),
    intel_steppings(HASWELL_L, ANY, bl!(SRBDS | VMSCAPE)),
    intel_steppings(HASWELL_G, ANY, bl!(SRBDS | VMSCAPE)),
    intel_steppings(HASWELL_X, ANY, bl!(MMIO | VMSCAPE)),
    intel_steppings(BROADWELL_D, ANY, bl!(MMIO | VMSCAPE)),
    intel_steppings(BROADWELL_X, ANY, bl!(MMIO | VMSCAPE)),
    intel_steppings(BROADWELL_G, ANY, bl!(SRBDS | VMSCAPE)),
    intel_steppings(BROADWELL, ANY, bl!(SRBDS | VMSCAPE)),
    intel_steppings(SKYLAKE_X, steppings(0x0, 0x5), bl!(MMIO | RETBLEED | GDS | VMSCAPE)),
    intel_steppings(SKYLAKE_X, ANY, bl!(MMIO | RETBLEED | GDS | ITS | VMSCAPE)),
    intel_steppings(SKYLAKE_L, ANY, bl!(MMIO | RETBLEED | GDS | SRBDS | VMSCAPE)),
    intel_steppings(SKYLAKE, ANY, bl!(MMIO | RETBLEED | GDS | SRBDS | VMSCAPE)),
    intel_steppings(KABYLAKE_L, steppings(0x0, 0xb), bl!(MMIO | RETBLEED | GDS | SRBDS | VMSCAPE)),
    intel_steppings(KABYLAKE_L, ANY, bl!(MMIO | RETBLEED | GDS | SRBDS | ITS | VMSCAPE)),
    intel_steppings(KABYLAKE, steppings(0x0, 0xc), bl!(MMIO | RETBLEED | GDS | SRBDS | VMSCAPE)),
    intel_steppings(KABYLAKE, ANY, bl!(MMIO | RETBLEED | GDS | SRBDS | ITS | VMSCAPE)),
    intel_steppings(CANNONLAKE_L, ANY, bl!(RETBLEED | VMSCAPE)),
    intel_steppings(ICELAKE_L, ANY, bl!(MMIO | MMIO_SBDS | RETBLEED | GDS | ITS | ITS_NATIVE_ONLY)),
    intel_steppings(ICELAKE_D, ANY, bl!(MMIO | GDS | ITS | ITS_NATIVE_ONLY)),
    intel_steppings(ICELAKE_X, ANY, bl!(MMIO | GDS | ITS | ITS_NATIVE_ONLY)),
    intel_steppings(COMETLAKE, ANY, bl!(MMIO | MMIO_SBDS | RETBLEED | GDS | ITS | VMSCAPE)),
    intel_steppings(COMETLAKE_L, steppings(0x0, 0x0), bl!(MMIO | RETBLEED | ITS | VMSCAPE)),
    intel_steppings(COMETLAKE_L, ANY, bl!(MMIO | MMIO_SBDS | RETBLEED | GDS | ITS | VMSCAPE)),
    intel_steppings(TIGERLAKE_L, ANY, bl!(GDS | ITS | ITS_NATIVE_ONLY)),
    intel_steppings(TIGERLAKE, ANY, bl!(GDS | ITS | ITS_NATIVE_ONLY)),
    intel_steppings(LAKEFIELD, ANY, bl!(MMIO | MMIO_SBDS | RETBLEED)),
    intel_steppings(ROCKETLAKE, ANY, bl!(MMIO | RETBLEED | GDS | ITS | ITS_NATIVE_ONLY)),
    intel_steppings(ALDERLAKE, ANY, bl!(RFDS | VMSCAPE)),
    intel_steppings(ALDERLAKE_L, ANY, bl!(RFDS | VMSCAPE)),
    intel_steppings(RAPTORLAKE, ANY, bl!(RFDS | VMSCAPE)),
    intel_steppings(RAPTORLAKE_P, ANY, bl!(RFDS | VMSCAPE)),
    intel_steppings(RAPTORLAKE_S, ANY, bl!(RFDS | VMSCAPE)),
    intel_steppings(METEORLAKE_L, ANY, bl!(VMSCAPE)),
    intel_steppings(ARROWLAKE_H, ANY, bl!(VMSCAPE)),
    intel_steppings(ARROWLAKE, ANY, bl!(VMSCAPE)),
    intel_steppings(ARROWLAKE_U, ANY, bl!(VMSCAPE)),
    intel_steppings(LUNARLAKE_M, ANY, bl!(VMSCAPE)),
    intel_steppings(SAPPHIRERAPIDS_X, ANY, bl!(VMSCAPE)),
    intel_steppings(GRANITERAPIDS_X, ANY, bl!(VMSCAPE)),
    intel_steppings(EMERALDRAPIDS_X, ANY, bl!(VMSCAPE)),
    intel_steppings(ATOM_GRACEMONT, ANY, bl!(RFDS | VMSCAPE)),
    intel_steppings(ATOM_TREMONT, ANY, bl!(MMIO | MMIO_SBDS | RFDS)),
    intel_steppings(ATOM_TREMONT_D, ANY, bl!(MMIO | RFDS)),
    intel_steppings(ATOM_TREMONT_L, ANY, bl!(MMIO | MMIO_SBDS | RFDS)),
    intel_steppings(ATOM_GOLDMONT, ANY, bl!(RFDS)),
    intel_steppings(ATOM_GOLDMONT_D, ANY, bl!(RFDS)),
    intel_steppings(ATOM_GOLDMONT_PLUS, ANY, bl!(RFDS)),
    intel_steppings(ATOM_CRESTMONT_X, ANY, bl!(VMSCAPE)),

    amd(0x15, bl!(RETBLEED)),
    amd(0x16, bl!(RETBLEED)),
    amd(0x17, bl!(RETBLEED | SMT_RSB | SRSO | VMSCAPE)),
    hygon(0x18, bl!(RETBLEED | SMT_RSB | SRSO | VMSCAPE)),
    amd(0x19, bl!(SRSO | TSA | VMSCAPE)),
];

/// `bad_spectre_microcode` (`intel.c:165-182`) over `spectre_bad_microcodes`
/// (`intel.c:141-163`), whose caller is Intel's (`intel.c:296-299`): a CPU
/// whose microcode is at or below its row's broke the speculation controls.
pub(crate) fn bad_spectre_microcode(id: &Ident, microcode: u32) -> bool {
    const ROWS: &[(u32, u32, u32)] = &[
        (KABYLAKE, 0x0B, 0x80),
        (KABYLAKE, 0x0A, 0x80),
        (KABYLAKE, 0x09, 0x80),
        (KABYLAKE_L, 0x0A, 0x80),
        (KABYLAKE_L, 0x09, 0x80),
        (SKYLAKE_X, 0x03, 0x0100013e),
        (SKYLAKE_X, 0x04, 0x0200003c),
        (BROADWELL, 0x04, 0x28),
        (BROADWELL_G, 0x01, 0x1b),
        (BROADWELL_D, 0x02, 0x14),
        (BROADWELL_D, 0x03, 0x07000011),
        (BROADWELL_X, 0x01, 0x0b000025),
        (HASWELL_L, 0x01, 0x21),
        (HASWELL_G, 0x01, 0x18),
        (HASWELL, 0x03, 0x23),
        (HASWELL_X, 0x02, 0x3b),
        (HASWELL_X, 0x04, 0x10),
        (IVYBRIDGE_X, 0x04, 0x42a),
        (SANDYBRIDGE_X, 0x06, 0x61b),
        (SANDYBRIDGE_X, 0x07, 0x712),
    ];
    id.vendor == Vendor::Intel
        && id.family == 6
        && ROWS
            .iter()
            .find(|&&(model, stepping, _)| model == id.model && stepping == id.stepping)
            .is_some_and(|&(_, _, bad)| microcode <= bad)
}

/// `amd_check_tsa_microcode` (`amd.c:472-515`): a CPU whose microcode is at or
/// above its row's. Every row keys a family 0x19 model `bsp_init_amd` names
/// Zen3 or Zen4 (`amd.c:619-632`), the only family TSA's row names
/// (`common.c:1342`).
pub(crate) fn tsa_microcode(id: &Ident, microcode: u32) -> bool {
    const ROWS: &[(u32, u32)] = &[
        (0xa0011, 0x0a0011d7),
        (0xa0012, 0x0a00123b),
        (0xa0082, 0x0a00820d),
        (0xa1011, 0x0a10114c),
        (0xa1012, 0x0a10124c),
        (0xa1081, 0x0a108109),
        (0xa2010, 0x0a20102e),
        (0xa2012, 0x0a201211),
        (0xa4041, 0x0a404108),
        (0xa5000, 0x0a500012),
        (0xa6012, 0x0a60120a),
        (0xa7041, 0x0a704108),
        (0xa7052, 0x0a705208),
        (0xa7080, 0x0a708008),
        (0xa70c0, 0x0a70c008),
        (0xaa002, 0x0aa00216),
    ];
    // `union zen_patch_rev` (`asm/cpu.h:80-90`) above its `rev` byte.
    let key = (id.family - 0xf) << 16 | (id.model >> 4) << 12 | (id.model & 0xf) << 4 | id.stepping;
    ROWS.iter().find(|&&(k, _)| k == key).is_some_and(|&(_, min)| microcode >= min)
}

/// `test_intel` for `PERF_MSR_SMI` (`arch/x86/events/msr.c:40-122`): an Intel
/// family 6 CPU of one of these models counts its SMIs in `MSR_SMI_COUNT`.
pub(crate) fn counts_smis(id: &Ident) -> bool {
    const MODELS: &[u32] = &[
        NEHALEM, NEHALEM_G, NEHALEM_EP, NEHALEM_EX,
        WESTMERE, WESTMERE_EP, WESTMERE_EX,
        SANDYBRIDGE, SANDYBRIDGE_X,
        IVYBRIDGE, IVYBRIDGE_X,
        HASWELL, HASWELL_X, HASWELL_L, HASWELL_G,
        BROADWELL, BROADWELL_D, BROADWELL_G, BROADWELL_X,
        SAPPHIRERAPIDS_X, EMERALDRAPIDS_X, GRANITERAPIDS_X, GRANITERAPIDS_D,
        ATOM_SILVERMONT, ATOM_SILVERMONT_D, ATOM_AIRMONT, ATOM_AIRMONT_NP,
        ATOM_GOLDMONT, ATOM_GOLDMONT_D, ATOM_GOLDMONT_PLUS,
        ATOM_TREMONT_D, ATOM_TREMONT, ATOM_TREMONT_L,
        XEON_PHI_KNL, XEON_PHI_KNM,
        SKYLAKE_L, SKYLAKE, SKYLAKE_X, KABYLAKE_L, KABYLAKE, COMETLAKE_L, COMETLAKE,
        ICELAKE_L, ICELAKE, ICELAKE_X, ICELAKE_D, TIGERLAKE_L, TIGERLAKE, ROCKETLAKE,
        ALDERLAKE, ALDERLAKE_L, ATOM_GRACEMONT, RAPTORLAKE, RAPTORLAKE_P, RAPTORLAKE_S,
        METEORLAKE, METEORLAKE_L,
    ];
    id.vendor == Vendor::Intel && id.family == 6 && MODELS.contains(&id.model)
}
