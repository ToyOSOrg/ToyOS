//! S0 of `issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`:
//! the T14 under its pinned Ubuntu, captured once, and that kernel on the TCG
//! model.
//!
//! `capture.sh` writes the T14's directory and `tcg.sh` the TCG model's.
//! [`T14::read`] and [`Tcg::read`] refuse a directory that lacks a file they
//! require, that is not the pinned kernel's, or whose sources disagree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The kernel the track pins, as `/proc/version` names it.
pub const KERNEL: &str = "6.8.0-142-generic";
/// The version of both of that kernel's packages.
pub const PACKAGES: &str = "6.8.0-142.142";
/// The sha256 of `/boot/config-6.8.0-142-generic`.
pub const CONFIG_SHA256: &str = "3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890";

/// Every file `capture.sh` writes.
pub const T14_FILES: &[&str] = &[
    "version.txt",
    "uname.txt",
    "cmdline.txt",
    "packages.txt",
    "config-sha256.txt",
    "config-hardening.txt",
    "mmap_rnd_bits.txt",
    "vulnerabilities.txt",
    "cpuinfo.txt",
    "kernel-log.txt",
    "dmi.txt",
    "cpuid.txt",
    "msr.txt",
];

/// Every file `tcg.sh` writes.
pub const TCG_FILES: &[&str] = &["qemu-version.txt", "console.txt"];

/// The `(leaf, subleaf)` pairs `capture.sh` reads on CPU 0.
pub const LEAVES: &[(u32, u32)] = &[
    (0, 0),
    (1, 0),
    (6, 0),
    (7, 0),
    (7, 1),
    (7, 2),
    (0xd, 0),
    (0xd, 1),
    (0x14, 0),
    (0x8000_0000, 0),
    (0x8000_0008, 0),
    (0x8000_0021, 0),
];

pub const IA32_SPEC_CTRL: u32 = 0x48;
pub const IA32_BIOS_SIGN_ID: u32 = 0x8b;
pub const IA32_ARCH_CAPABILITIES: u32 = 0x10a;
pub const IA32_TSX_FORCE_ABORT: u32 = 0x10f;
pub const IA32_TSX_CTRL: u32 = 0x122;
pub const IA32_MCU_OPT_CTRL: u32 = 0x123;

/// The MSRs `capture.sh` reads on every CPU whatever the CPU enumerates.
pub const MSRS: &[u32] = &[
    IA32_BIOS_SIGN_ID,
    IA32_SPEC_CTRL,
    IA32_ARCH_CAPABILITIES,
    IA32_MCU_OPT_CTRL,
];

/// `ARCH_CAPABILITIES` bit 7, TSX_CTRL_MSR: `IA32_TSX_CTRL` exists.
const ARCH_CAP_TSX_CTRL_MSR: u64 = 1 << 7;
/// CPUID.(7,0):EDX bits 11 and 13, RTM_ALWAYS_ABORT and TSX_FORCE_ABORT, both
/// of which Linux requires before it touches `IA32_TSX_FORCE_ABORT`.
const TSX_FORCE_ABORT_MSR: u32 = 1 << 11 | 1 << 13;
/// CPUID.(7,0):EDX bit 29: `IA32_ARCH_CAPABILITIES` exists.
const ARCH_CAPABILITIES_BIT: u32 = 1 << 29;

const VULNERABILITY: &str = "/sys/devices/system/cpu/vulnerabilities/";

/// One CPUID answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regs {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

/// What S1 decides a CPU's vulnerabilities from, as Linux reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// CPUID.0's EBX, EDX, ECX.
    pub vendor: [u8; 12],
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    /// `IA32_BIOS_SIGN_ID` bits 63:32, the revision `/proc/cpuinfo` shows.
    pub microcode: u32,
    pub leaf_1: Regs,
    pub leaf_7_0: Regs,
    pub leaf_7_2: Regs,
    /// CPUID.0x80000008:EBX where CPUID.0x80000000:EAX reaches it, else 0.
    pub leaf_8000_0008_ebx: u32,
    /// CPUID.0x80000021:EAX where CPUID.0x80000000:EAX reaches it, else 0.
    pub leaf_8000_0021_eax: u32,
    /// `IA32_ARCH_CAPABILITIES` where CPUID.(7,0):EDX enumerates it, else 0.
    pub arch_capabilities: u64,
}

/// The T14's capture.
#[derive(Debug)]
pub struct T14 {
    pub facts: Facts,
    /// `/proc/version`.
    pub version: String,
    /// Each vulnerabilities file's name and line.
    pub lines: BTreeMap<String, String>,
    /// Each MSR `capture.sh` read, by CPU number.
    pub msrs: BTreeMap<u32, BTreeMap<u32, u64>>,
    pub mmap_rnd_bits: u32,
}

/// The TCG model's capture: the T14's kernel under `.github/qemu-version`'s QEMU.
#[derive(Debug)]
pub struct Tcg {
    pub lines: BTreeMap<String, String>,
}

/// A directory's files, `None` for one that is not there.
fn from_dir(dir: &Path) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| match std::fs::read_to_string(dir.join(name)) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", dir.join(name).display()),
    }
}

/// Every file of `names`, or a refusal naming the first that is missing.
fn files(
    read: &dyn Fn(&str) -> Option<String>,
    names: &[&'static str],
) -> Result<BTreeMap<&'static str, String>, String> {
    names
        .iter()
        .map(|&n| read(n).map(|t| (n, t)).ok_or(format!("{n} is missing")))
        .collect()
}

fn hex<T: TryFrom<u64>>(word: &str, what: &str) -> Result<T, String> {
    let bad = || format!("{what}: {word:?} is not hex");
    let digits = word.strip_prefix("0x").unwrap_or(word);
    let v = u64::from_str_radix(digits, 16).map_err(|_| bad())?;
    T::try_from(v).map_err(|_| bad())
}

/// `grep . <vulnerabilities>/*`'s lines among others, each name once.
fn vulnerabilities<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for l in lines {
        let Some(rest) = l.strip_prefix(VULNERABILITY) else {
            continue;
        };
        let (name, line) = rest
            .split_once(':')
            .ok_or(format!("{l:?} is not a vulnerabilities line"))?;
        if out.insert(name.to_string(), line.to_string()).is_some() {
            return Err(format!("vulnerability {name} twice"));
        }
    }
    if out.is_empty() {
        return Err("no vulnerabilities line".into());
    }
    Ok(out)
}

/// `file`, which must read exactly `want`.
fn is(f: &BTreeMap<&str, String>, file: &str, want: &str) -> Result<(), String> {
    match f[file] == want {
        true => Ok(()),
        false => Err(format!("{file} is {:?}, not {want:?}", f[file])),
    }
}

/// `/proc/cpuinfo`'s blocks by processor, each as `key\t: value`.
fn cpuinfo(text: &str) -> Result<BTreeMap<u32, BTreeMap<&str, &str>>, String> {
    let mut cpus = BTreeMap::new();
    for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let fields: BTreeMap<&str, &str> = block
            .lines()
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let n = fields
            .get("processor")
            .and_then(|p| p.parse().ok())
            .ok_or("a cpuinfo block names no processor")?;
        if cpus.insert(n, fields).is_some() {
            return Err(format!("cpuinfo: processor {n} twice"));
        }
    }
    Ok(cpus)
}

/// The one value every CPU's `key` holds.
fn same<T: PartialEq + Copy + std::fmt::Debug>(
    per_cpu: impl Iterator<Item = (u32, T)>,
    what: &str,
) -> Result<T, String> {
    let mut first: Option<T> = None;
    for (cpu, v) in per_cpu {
        match first {
            None => first = Some(v),
            Some(f) if f == v => {}
            Some(f) => return Err(format!("{what}: CPU {cpu} reads {v:?}, not {f:?}")),
        }
    }
    first.ok_or(format!("{what}: no CPU"))
}

fn cpuid(text: &str) -> Result<BTreeMap<(u32, u32), Regs>, String> {
    let mut leaves = BTreeMap::new();
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        let [leaf, sub, a, b, c, d] = w[..] else {
            return Err(format!("cpuid.txt: {l:?} is not six words"));
        };
        let what = "cpuid.txt";
        let regs = Regs {
            eax: hex(a, what)?,
            ebx: hex(b, what)?,
            ecx: hex(c, what)?,
            edx: hex(d, what)?,
        };
        let key = (hex(leaf, what)?, hex(sub, what)?);
        if leaves.insert(key, regs).is_some() {
            return Err(format!("cpuid.txt: leaf {:#x}.{} twice", key.0, key.1));
        }
    }
    Ok(leaves)
}

fn msrs(text: &str) -> Result<BTreeMap<u32, BTreeMap<u32, u64>>, String> {
    let mut msrs: BTreeMap<u32, BTreeMap<u32, u64>> = BTreeMap::new();
    for l in text.lines() {
        let [msr, cpu, value] = l.split_whitespace().collect::<Vec<_>>()[..] else {
            return Err(format!("msr.txt: {l:?} is not three words"));
        };
        let msr = hex(msr, "msr.txt")?;
        let cpu = cpu
            .parse()
            .map_err(|_| format!("msr.txt: {cpu:?} is not a CPU"))?;
        if msrs
            .entry(msr)
            .or_default()
            .insert(cpu, hex(value, "msr.txt")?)
            .is_some()
        {
            return Err(format!("msr.txt: MSR {msr:#x} on CPU {cpu} twice"));
        }
    }
    Ok(msrs)
}

impl T14 {
    pub fn read(dir: &Path) -> Result<T14, String> {
        T14::parse(&from_dir(dir))
    }

    /// The capture `read` answers for, file by file.
    pub fn parse(read: &dyn Fn(&str) -> Option<String>) -> Result<T14, String> {
        let f = files(read, T14_FILES)?;
        let version = f["version.txt"].trim_end().to_string();
        if !version.starts_with(&format!("Linux version {KERNEL} ")) || version.contains('\n') {
            return Err(format!("version.txt is {version:?}, not {KERNEL}'s"));
        }
        is(
            &f,
            "packages.txt",
            &format!("linux-image-{KERNEL}\t{PACKAGES}\nlinux-modules-{KERNEL}\t{PACKAGES}\n"),
        )?;
        is(
            &f,
            "config-sha256.txt",
            &format!("{CONFIG_SHA256}  /boot/config-{KERNEL}\n"),
        )?;
        let mmap_rnd_bits = f["mmap_rnd_bits.txt"]
            .trim_end()
            .strip_prefix("vm.mmap_rnd_bits = ")
            .and_then(|n| n.parse().ok())
            .ok_or(format!("mmap_rnd_bits.txt is {:?}", f["mmap_rnd_bits.txt"]))?;
        let lines = vulnerabilities(f["vulnerabilities.txt"].lines())?;

        let info = cpuinfo(&f["cpuinfo.txt"])?;
        let field = |key: &'static str| {
            same(
                info.iter().map(|(&c, b)| (c, b.get(key).copied())),
                &format!("cpuinfo {key}"),
            )
            .and_then(|v| v.ok_or(format!("cpuinfo names no {key}")))
        };
        let cpus: BTreeSet<u32> = info.keys().copied().collect();

        let leaves = cpuid(&f["cpuid.txt"])?;
        let leaf = |l: u32, s: u32| {
            leaves
                .get(&(l, s))
                .copied()
                .ok_or(format!("cpuid.txt holds no leaf {l:#x}.{s}"))
        };
        for &(l, s) in LEAVES {
            leaf(l, s)?;
        }

        let msrs = msrs(&f["msr.txt"])?;
        let msr = |m: u32| -> Result<u64, String> {
            let per_cpu = msrs.get(&m).ok_or(format!("msr.txt holds no MSR {m:#x}"))?;
            same(
                per_cpu.iter().map(|(&c, &v)| (c, v)),
                &format!("MSR {m:#x}"),
            )
        };
        for &m in MSRS {
            msrs.get(&m).ok_or(format!("msr.txt holds no MSR {m:#x}"))?;
        }
        for (m, per_cpu) in &msrs {
            let read_on: BTreeSet<u32> = per_cpu.keys().copied().collect();
            if read_on != cpus {
                return Err(format!("MSR {m:#x} read on CPUs {read_on:?}, not {cpus:?}"));
            }
        }

        let l0 = leaf(0, 0)?;
        let mut vendor = [0; 12];
        for (i, r) in [l0.ebx, l0.edx, l0.ecx].into_iter().enumerate() {
            vendor[i * 4..][..4].copy_from_slice(&r.to_le_bytes());
        }
        let leaf_1 = leaf(1, 0)?;
        let base = leaf_1.eax >> 8 & 0xf;
        let family = match base {
            0xf => base + (leaf_1.eax >> 20 & 0xff),
            _ => base,
        };
        let model = match family >= 6 {
            true => (leaf_1.eax >> 4 & 0xf) | (leaf_1.eax >> 16 & 0xf) << 4,
            false => leaf_1.eax >> 4 & 0xf,
        };
        let stepping = leaf_1.eax & 0xf;
        let decimal = |key: &'static str| -> Result<u32, String> {
            field(key)?
                .parse()
                .map_err(|_| format!("cpuinfo {key} is not a number"))
        };
        for (key, cpuid, linux) in [
            ("cpu family", family, decimal("cpu family")?),
            ("model", model, decimal("model")?),
            ("stepping", stepping, decimal("stepping")?),
        ] {
            if cpuid != linux {
                return Err(format!(
                    "CPUID.1 decodes {key} {cpuid}, and cpuinfo reads {linux}"
                ));
            }
        }
        if field("vendor_id")?.as_bytes() != vendor {
            return Err(format!(
                "CPUID.0 names {:?}, and cpuinfo reads {:?}",
                String::from_utf8_lossy(&vendor),
                field("vendor_id")?
            ));
        }
        let microcode: u32 = hex(field("microcode")?, "cpuinfo microcode")?;
        let sign_id = (msr(IA32_BIOS_SIGN_ID)? >> 32) as u32;
        if sign_id != microcode {
            return Err(format!(
                "IA32_BIOS_SIGN_ID reads revision {sign_id:#x}, and cpuinfo {microcode:#x}"
            ));
        }

        let leaf_7_0 = leaf(7, 0)?;
        let arch = msr(IA32_ARCH_CAPABILITIES)?;
        if arch & ARCH_CAP_TSX_CTRL_MSR != 0 {
            msr(IA32_TSX_CTRL)?;
        }
        if leaf_7_0.edx & TSX_FORCE_ABORT_MSR == TSX_FORCE_ABORT_MSR {
            msr(IA32_TSX_FORCE_ABORT)?;
        }
        let max_ext = leaf(0x8000_0000, 0)?.eax;
        let ext = |l: u32| -> Result<Option<Regs>, String> {
            match max_ext >= l {
                true => leaf(l, 0).map(Some),
                false => Ok(None),
            }
        };
        let facts = Facts {
            vendor,
            family,
            model,
            stepping,
            microcode,
            leaf_1,
            leaf_7_0,
            leaf_7_2: leaf(7, 2)?,
            leaf_8000_0008_ebx: ext(0x8000_0008)?.map_or(0, |r| r.ebx),
            leaf_8000_0021_eax: ext(0x8000_0021)?.map_or(0, |r| r.eax),
            arch_capabilities: match leaf_7_0.edx & ARCH_CAPABILITIES_BIT {
                0 => 0,
                _ => arch,
            },
        };
        Ok(T14 {
            facts,
            version,
            lines,
            msrs,
            mmap_rnd_bits,
        })
    }
}

/// `.github/qemu-version`'s version.
pub fn qemu_pin() -> &'static str {
    include_str!("../../.github/qemu-version")
        .lines()
        .rfind(|l| !l.starts_with('#'))
        .expect(".github/qemu-version names a version")
}

impl Tcg {
    pub fn read(dir: &Path, t14: &T14) -> Result<Tcg, String> {
        Tcg::parse(&from_dir(dir), t14)
    }

    /// The TCG model's capture `read` answers for, held to the T14's kernel.
    pub fn parse(read: &dyn Fn(&str) -> Option<String>, t14: &T14) -> Result<Tcg, String> {
        let f = files(read, TCG_FILES)?;
        let qemu = format!("QEMU emulator version {}", qemu_pin());
        if f["qemu-version.txt"].lines().next() != Some(&qemu) {
            return Err(format!("qemu-version.txt does not open with {qemu:?}"));
        }
        let console: Vec<&str> = f["console.txt"]
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .collect();
        let versions: Vec<&&str> = console
            .iter()
            .filter(|l| l.starts_with("Linux version "))
            .collect();
        if versions[..] != [&t14.version.as_str()] {
            return Err(format!(
                "console.txt's /proc/version is {versions:?}, not the T14's"
            ));
        }
        let lines = vulnerabilities(console.into_iter())?;
        if !lines.keys().eq(t14.lines.keys()) {
            return Err(format!(
                "console.txt reads vulnerabilities {:?}, and the T14 {:?}",
                lines.keys().collect::<Vec<_>>(),
                t14.lines.keys().collect::<Vec<_>>()
            ));
        }
        Ok(Tcg { lines })
    }
}

#[cfg(test)]
mod tests;
