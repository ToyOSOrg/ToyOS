//! The judge of the T14 LLVM bar (`issues/build/toyos-builds-itself.md`).
//!
//! On the T14, `t14/sampler.sh <dir>` runs as root and `t14/driver.sh <dir>` as
//! the build's user, `<dir>` holding `t14/block.txt`. The judge reads what they
//! leave there, and the T14 capture of
//! `issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-are-not-committed.md`:
//! its `/proc/version`, vulnerabilities and `/proc/cpuinfo` text.
//!
//! A stage-3 span `s3-<k>`, `k >= 1`, is a valid sample when:
//! - `samples-s3-<k>.log` opens with a `read=start` row and closes with a
//!   `read=end` row, every row between is `read=timed`, and every row is
//!   `phase=s3-<k>`;
//! - every row reads every element of [`ENVELOPE`], [`ENVELOPE_EVERY_CPU`],
//!   [`LINUX`] and [`LINUX_EVERY_CPU`] at its value, and [`MICROCODE`] on every
//!   CPU;
//! - consecutive rows start at most [`MAX_GAP_MS`] apart;
//! - `s3-<k>-time.txt`, GNU `time -v`'s, reads exit status 0 and file system
//!   inputs 0;
//! - `s3-<k>-machine-start.txt` and `s3-<k>-machine-end.txt` both read kernel
//!   [`KERNEL`], one command line, one BIOS version and one boot, and the
//!   capture's `/proc/version`, vulnerabilities and microcode lines;
//! - it is warm: `s3-<k-1>` exited 0, and its end read precedes this span's
//!   start read.
//!
//! The bar is the shortest wall of the valid samples once there are
//! [`SAMPLES`] of them.

use std::collections::BTreeMap;

/// The T14's logical CPUs, every one of which a per-CPU element is read on.
pub const CPUS: usize = 8;
/// How many valid samples the bar is the best of.
pub const SAMPLES: usize = 3;
/// The longest a span may go between the starts of two reads.
pub const MAX_GAP_MS: u64 = 61_000;
/// The kernel the capture pins.
pub const KERNEL: &str = "6.8.0-142-generic";
/// The microcode revision, `IA32_BIOS_SIGN_ID` (0x8B) bits 63:32, on every CPU.
pub const MICROCODE: u32 = 0xbe;

/// The power envelope's package elements, a ToyOS run's as much as Linux's:
/// the sampler's key and the value it reads.
pub const ENVELOPE: &[(&str, &str)] = &[
    ("ac", "1"),
    ("platform_profile", "performance"),
    // MSR_PKG_POWER_LIMIT: PL1 64 W over 28 s, PL2 64 W over 2.44 ms, both
    // enabled, unlocked.
    ("msr_610", "0042820000dd8200"),
    // The package limit through MMIO, MCHBAR + 0x59A0: PL1 20 W over 28 s, PL2
    // 64 W over 2.44 ms.
    ("mmio_59a0", "0042820000dd80a0"),
    // MSR_VR_CURRENT_CONFIG: the peak limit, 121 W.
    ("msr_601", "00000000000003c8"),
    // IA32_HWP_REQUEST_PKG.
    ("msr_772", "000000008000ff01"),
];

/// The power envelope's per-CPU elements, each read on every CPU.
pub const ENVELOPE_EVERY_CPU: &[(&str, &str)] = &[
    // IA32_PM_ENABLE: HWP on.
    ("msr_770", "0000000000000001"),
    // IA32_HWP_REQUEST: minimum 4, maximum 42, desired 0, EPP 128, activity
    // window 0, package control off.
    ("msr_774", "0000000080002a04"),
    // IA32_ENERGY_PERF_BIAS.
    ("msr_1b0", "0000000000000006"),
    // IA32_MISC_ENABLE bit 38 clear: turbo enabled.
    ("msr_1a0_b38", "0"),
];

/// How Linux reaches the envelope, read back beside it: powercap's limits in
/// µW (PL1, PL2, peak) and windows in µs (PL1, PL2) for the MSR and the MMIO
/// package limit, and intel_pstate.
pub const LINUX: &[(&str, &str)] = &[
    ("rapl_msr", "64000000,64000000,121000000,27983872,2440"),
    ("rapl_mmio", "20000000,64000000,121000000,27983872,2440"),
    ("pstate", "active"),
    ("no_turbo", "0"),
    ("hwp_dynamic_boost", "0"),
];

/// Linux's per-CPU cpufreq settings, each read on every CPU.
pub const LINUX_EVERY_CPU: &[(&str, &str)] = &[
    ("governor", "powersave"),
    ("epp", "balance_performance"),
    ("min_khz", "400000"),
    ("max_khz", "4200000"),
];

const VULNERABILITY: &str = "/sys/devices/system/cpu/vulnerabilities/";
const BIOS: &str = "/sys/class/dmi/id/bios_version:";
const BOOT: &str = "/proc/sys/kernel/random/boot_id:";

/// Every key a sampler row must carry and the value it must read.
pub fn wanted() -> Vec<(&'static str, String)> {
    let every_cpu = |v: &str| [v; CPUS].join(",");
    let mut want: Vec<(&'static str, String)> = ENVELOPE
        .iter()
        .chain(LINUX)
        .map(|&(k, v)| (k, v.to_string()))
        .collect();
    want.extend(
        ENVELOPE_EVERY_CPU
            .iter()
            .chain(LINUX_EVERY_CPU)
            .map(|&(k, v)| (k, every_cpu(v))),
    );
    want.push(("msr_8b", every_cpu(&format!("{MICROCODE:08x}00000000"))));
    want
}

/// What Linux's T14 capture read that a span's machine reads must repeat.
#[derive(Debug)]
pub struct Capture {
    version: String,
    vulnerabilities: Vec<String>,
    microcode: Vec<String>,
}

impl Capture {
    /// Takes the `/proc/version`, vulnerabilities and microcode lines out of
    /// the capture's text, whatever else it holds.
    pub fn parse(text: &str) -> Result<Capture, String> {
        let versions: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("Linux version "))
            .collect();
        let [version] = versions[..] else {
            return Err(format!(
                "the capture holds {} /proc/version lines, not 1",
                versions.len()
            ));
        };
        let mut vulnerabilities: Vec<String> = text
            .lines()
            .filter(|l| l.starts_with(VULNERABILITY))
            .map(String::from)
            .collect();
        vulnerabilities.sort();
        if vulnerabilities.is_empty() {
            return Err("the capture holds no vulnerabilities line".into());
        }
        let microcode: Vec<String> = text.lines().filter_map(microcode).collect();
        if microcode.len() != CPUS {
            return Err(format!(
                "the capture holds {} microcode lines, not {CPUS}",
                microcode.len()
            ));
        }
        Ok(Capture {
            version: version.into(),
            vulnerabilities,
            microcode,
        })
    }
}

/// One machine read, as `driver.sh`'s `machine` writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub version: String,
    pub cmdline: String,
    pub bios: String,
    boot: String,
    vulnerabilities: Vec<String>,
    microcode: Vec<String>,
}

impl Machine {
    pub fn parse(text: &str) -> Result<Machine, String> {
        let mut lines = text.lines();
        let version = lines
            .next()
            .filter(|l| l.starts_with("Linux version "))
            .ok_or("its first line is not /proc/version")?;
        let cmdline = lines
            .next()
            .filter(|l| !l.is_empty())
            .ok_or("its second line is not /proc/cmdline")?;
        let (mut vulnerabilities, mut microcodes, mut bios, mut boot) =
            (Vec::new(), Vec::new(), None, None);
        for line in lines {
            if line.starts_with(VULNERABILITY) {
                vulnerabilities.push(line.to_string());
            } else if let Some(m) = microcode(line) {
                microcodes.push(m);
            } else if let Some(b) = line.strip_prefix(BIOS) {
                once(&mut bios, b, "BIOS version")?;
            } else if let Some(b) = line.strip_prefix(BOOT) {
                once(&mut boot, b, "boot_id")?;
            } else {
                return Err(format!("it holds a line nothing reads: {line:?}"));
            }
        }
        vulnerabilities.sort();
        if vulnerabilities.is_empty() {
            return Err("it holds no vulnerabilities line".into());
        }
        if microcodes.len() != CPUS {
            return Err(format!(
                "it holds {} microcode lines, not {CPUS}",
                microcodes.len()
            ));
        }
        Ok(Machine {
            version: version.into(),
            cmdline: cmdline.into(),
            bios: bios.ok_or("it holds no BIOS version")?,
            boot: boot.ok_or("it holds no boot_id")?,
            vulnerabilities,
            microcode: microcodes,
        })
    }
}

fn once(slot: &mut Option<String>, value: &str, what: &str) -> Result<(), String> {
    match slot.replace(value.to_string()) {
        None if !value.is_empty() => Ok(()),
        None => Err(format!("its {what} is empty")),
        Some(_) => Err(format!("it holds two {what} lines")),
    }
}

/// The value of a `/proc/cpuinfo` `microcode` line.
fn microcode(line: &str) -> Option<String> {
    let value = line
        .strip_prefix("microcode")?
        .trim_start()
        .strip_prefix(':')?;
    Some(value.trim().to_string())
}

/// Milliseconds since the epoch of a `date -u +%FT%T.%3NZ` stamp.
pub fn utc_ms(stamp: &str) -> Result<u64, String> {
    let bad = || format!("{stamp:?} is not a UTC stamp");
    let b = stamp.as_bytes();
    let separators = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (23, b'Z'),
    ];
    if b.len() != 24 || separators.iter().any(|&(i, c)| b[i] != c) {
        return Err(bad());
    }
    let n = |from: usize, to: usize| -> Result<i64, String> {
        let digits = &stamp[from..to];
        if digits.bytes().all(|c| c.is_ascii_digit()) {
            digits.parse().map_err(|_| bad())
        } else {
            Err(bad())
        }
    };
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss, ms) = (n(11, 13)?, n(14, 16)?, n(17, 19)?, n(20, 23)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return Err(bad());
    }
    // Days from 1970-01-01 in the proleptic Gregorian calendar, the year
    // taken to start in March so the leap day ends it.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from(((days * 24 + hh) * 60 + mm) * 60 + ss)
        .map(|s| s * 1000 + ms as u64)
        .map_err(|_| bad())
}

/// Centiseconds of GNU time's `Elapsed (wall clock)` value, `m:ss.cc` or
/// `h:mm:ss.cc`.
pub fn wall_cs(value: &str) -> Result<u64, String> {
    let bad = || format!("{value:?} is not a GNU time wall clock");
    let (whole, cc) = value.split_once('.').ok_or_else(bad)?;
    let numbers: Vec<u64> = whole
        .split(':')
        .chain([cc])
        .map(|p| {
            if !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) {
                p.parse().ok()
            } else {
                None
            }
        })
        .collect::<Option<_>>()
        .ok_or_else(bad)?;
    let (h, m, s, c) = match numbers[..] {
        [m, s, c] => (0, m, s, c),
        [h, m, s, c] => (h, m, s, c),
        _ => return Err(bad()),
    };
    if cc.len() != 2 || s > 59 || (h > 0 && m > 59) {
        return Err(bad());
    }
    Ok(((h * 60 + m) * 60 + s) * 100 + c)
}

/// `wall_cs`'s inverse, in GNU time's form.
pub fn show_wall(cs: u64) -> String {
    let (s, c) = (cs / 100, cs % 100);
    match s / 3600 {
        0 => format!("{}:{:02}.{c:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}.{c:02}", s / 60 % 60, s % 60),
    }
}

/// A row's `key=value` fields.
fn fields(row: &str) -> Result<BTreeMap<&str, &str>, String> {
    let mut map = BTreeMap::new();
    for token in row.split_whitespace() {
        let (k, v) = token
            .split_once('=')
            .ok_or_else(|| format!("{token:?} is not key=value"))?;
        if map.insert(k, v).is_some() {
            return Err(format!("{k} twice"));
        }
    }
    Ok(map)
}

/// The rows of one span's sampler log against the envelope, its tags and its
/// cadence; the start read's time when the start and end reads are both there.
fn check_log(text: &str, span: &str, refusals: &mut Vec<String>) -> Option<u64> {
    let want = wanted();
    let rows: Vec<&str> = text.lines().collect();
    if rows.len() < 2 {
        refusals.push(format!(
            "its sampler log holds {} rows, and a span is read at its start and its end",
            rows.len()
        ));
    }
    let (mut first, mut prev, mut ends): (Option<u64>, Option<u64>, _) =
        (None, None, (false, false));
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        let f = match fields(row) {
            Ok(f) => f,
            Err(e) => {
                refusals.push(format!("row {n}: {e}"));
                continue;
            }
        };
        let tag = match i {
            0 => "start",
            _ if n == rows.len() => "end",
            _ => "timed",
        };
        match f.get("read") {
            Some(&t) if t == tag => {
                ends.0 |= tag == "start";
                ends.1 |= tag == "end";
            }
            got => refusals.push(format!(
                "row {n}: read={}, not read={tag}",
                got.unwrap_or(&"<none>")
            )),
        }
        if f.get("phase") != Some(&span) {
            refusals.push(format!(
                "row {n}: phase={}, not phase={span}",
                f.get("phase").unwrap_or(&"<none>")
            ));
        }
        for (k, v) in &want {
            if f.get(k) != Some(&v.as_str()) {
                refusals.push(format!(
                    "row {n}: {k}={}, not {v}",
                    f.get(k).unwrap_or(&"<none>")
                ));
            }
        }
        match f.get("utc").map(|u| utc_ms(u)) {
            Some(Ok(t)) => {
                if let Some(p) = prev {
                    match t.checked_sub(p) {
                        Some(gap) if gap <= MAX_GAP_MS => {}
                        Some(gap) => {
                            refusals.push(format!("row {n}: {gap} ms after the read before it"))
                        }
                        None => refusals.push(format!("row {n}: earlier than the read before it")),
                    }
                }
                first.get_or_insert(t);
                prev = Some(t);
            }
            Some(Err(e)) => refusals.push(format!("row {n}: {e}")),
            None => refusals.push(format!("row {n}: no utc")),
        }
    }
    first.filter(|_| ends == (true, true))
}

/// GNU `time -v`'s `key: value` lines.
fn gnu_time(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .filter_map(|l| l.rsplit_once(": "))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}

/// Exit status 0 and file system inputs 0, or why not; the wall if both hold.
fn check_time(text: Option<&str>, name: &str, refusals: &mut Vec<String>) -> Option<u64> {
    let Some(text) = text else {
        refusals.push(format!("{name} has no time file: its ninja did not finish"));
        return None;
    };
    let time = gnu_time(text);
    let mut ok = true;
    for (key, want) in [("Exit status", "0"), ("File system inputs", "0")] {
        if time.get(key) != Some(&want) {
            refusals.push(format!(
                "{name}: {key} {}, not {want}",
                time.get(key).unwrap_or(&"<none>")
            ));
            ok = false;
        }
    }
    match time
        .get("Elapsed (wall clock) time (h:mm:ss or m:ss)")
        .map(|w| wall_cs(w))
    {
        Some(Ok(wall)) if ok => Some(wall),
        Some(Ok(_)) => None,
        Some(Err(e)) => {
            refusals.push(format!("{name}: {e}"));
            None
        }
        None => {
            refusals.push(format!("{name}: no wall clock"));
            None
        }
    }
}

/// One stage-3 span's judgement.
#[derive(Debug)]
pub struct Verdict {
    pub span: String,
    pub wall_cs: Option<u64>,
    pub machine: Option<Machine>,
    pub refusals: Vec<String>,
}

impl Verdict {
    pub fn valid(&self) -> bool {
        self.refusals.is_empty() && self.wall_cs.is_some()
    }
}

/// Judges `s3-<k>`, `k >= 1`, reading the run directory through `read`, which
/// answers `None` for a file that is not there.
pub fn judge_span(read: &dyn Fn(&str) -> Option<String>, capture: &Capture, k: u32) -> Verdict {
    assert!(
        k >= 1,
        "s3-0 is the line the first span follows, never a sample"
    );
    let span = format!("s3-{k}");
    let prev = format!("s3-{}", k - 1);
    let mut refusals = Vec::new();

    let start = match read(&format!("samples-{span}.log")) {
        Some(log) => check_log(&log, &span, &mut refusals),
        None => {
            refusals.push("no sampler log".into());
            None
        }
    };
    let wall_cs = check_time(
        read(&format!("{span}-time.txt")).as_deref(),
        &span,
        &mut refusals,
    );

    let mut reads = Vec::new();
    for end in ["start", "end"] {
        let name = format!("{span}-machine-{end}.txt");
        match read(&name).map(|t| Machine::parse(&t)) {
            Some(Ok(m)) => reads.push(m),
            Some(Err(e)) => refusals.push(format!("{name}: {e}")),
            None => refusals.push(format!("no {name}")),
        }
    }
    for m in &reads {
        if m.version.split_whitespace().nth(2) != Some(KERNEL) {
            refusals.push(format!("the kernel is not {KERNEL}: {:?}", m.version));
        }
        let each = format!("{MICROCODE:#x}");
        let checks = [
            (
                m.version == capture.version,
                "its /proc/version is not the capture's",
            ),
            (
                m.vulnerabilities == capture.vulnerabilities,
                "its vulnerabilities lines are not the capture's",
            ),
            (
                m.microcode == capture.microcode,
                "its microcode lines are not the capture's",
            ),
            (
                m.microcode.iter().all(|r| *r == each),
                "a CPU's microcode is not the recipe's",
            ),
        ];
        refusals.extend(
            checks
                .iter()
                .filter(|(ok, _)| !ok)
                .map(|(_, why)| why.to_string()),
        );
    }
    if let [first, last] = &reads[..] {
        for (same, what) in [
            (first.cmdline == last.cmdline, "command line"),
            (first.bios == last.bios, "BIOS version"),
            (first.boot == last.boot, "boot"),
        ] {
            if !same {
                refusals.push(format!(
                    "the {what} changed between the start and end machine reads"
                ));
            }
        }
    }

    let prev_exited = check_time(
        read(&format!("{prev}-time.txt")).as_deref(),
        &prev,
        &mut refusals,
    )
    .is_some();
    let prev_end = read(&format!("samples-{prev}.log")).and_then(|log| {
        let f = fields(log.lines().last()?).ok()?;
        if f.get("read") != Some(&"end") {
            return None;
        }
        utc_ms(f.get("utc")?).ok()
    });
    let prev_end = prev_end.filter(|_| prev_exited);
    match (prev_end, start) {
        (Some(p), Some(start)) if p < start => {}
        (Some(_), Some(_)) => refusals.push(format!(
            "{prev}'s end read does not precede this span's start read"
        )),
        (None, _) => refusals.push(format!(
            "not warm: {prev} has no end read after a build that exited 0"
        )),
        (Some(_), None) => {}
    }

    Verdict {
        span,
        wall_cs,
        machine: reads.into_iter().next(),
        refusals,
    }
}

/// Every stage-3 span a run left, judged, and the bar if it is set.
pub struct Run {
    pub verdicts: Vec<Verdict>,
    pub bar_cs: Option<u64>,
}

/// Judges `s3-1` onwards until a span that left no file at all.
pub fn judge(read: &dyn Fn(&str) -> Option<String>, capture: &Capture) -> Run {
    let left_any = |k: u32| {
        [
            format!("samples-s3-{k}.log"),
            format!("s3-{k}-time.txt"),
            format!("s3-{k}-machine-start.txt"),
        ]
        .iter()
        .any(|f| read(f).is_some())
    };
    let verdicts: Vec<Verdict> = (1..)
        .take_while(|&k| left_any(k))
        .map(|k| judge_span(read, capture, k))
        .collect();
    let walls: Vec<u64> = verdicts
        .iter()
        .filter(|v| v.valid())
        .filter_map(|v| v.wall_cs)
        .collect();
    let bar_cs = (walls.len() >= SAMPLES)
        .then(|| walls.iter().copied().min())
        .flatten();
    Run { verdicts, bar_cs }
}

#[cfg(test)]
mod tests;
