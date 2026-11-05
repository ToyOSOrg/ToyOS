//! The kernel's interrupt census, and the windows a `mask-windows` kernel
//! reports, read back on the host.
//!
//! The kernel says `irq: cpuN timer=… kick=… …` per online CPU at its stop
//! (`kernel/src/census.rs`).

use std::collections::BTreeMap;

/// The census's source names, in the order `kernel/src/irq_census.rs` prints
/// them. The kernel's `Source::NAMES` is the definition; this is the host's copy
/// and [`census_sources`] refuses a line whose fields are not exactly these, so
/// a source added on one side and not the other is a red rather than a silently
/// dropped column.
pub const SOURCES: [&str; 9] = ["timer", "kick", "userdev", "dmafault", "rtc", "tlb", "nmi", "spurious", "unclaimed"];

/// Whether one line is a census line whose sources are exactly [`SOURCES`],
/// or `None` for a line that is no census.
///
/// Anything before `irq: cpu` is ignored, so the same reader takes a raw guest
/// line, a kernel record's line and a `[serial N]` echo of either.
pub fn census_sources(line: &str) -> Option<Result<(), String>> {
    let rest = line.split("irq: cpu").nth(1)?;
    let mut fields = rest.split_whitespace();
    let read = (|| {
        fields
            .next()
            .ok_or_else(|| format!("no cpu number in {rest:?}"))?
            .parse::<u32>()
            .map_err(|_| format!("unreadable cpu number in {rest:?}"))?;
        let mut got = Vec::new();
        for field in fields {
            let (name, value) = field
                .split_once('=')
                .ok_or_else(|| format!("field {field:?} is not name=value in {rest:?}"))?;
            value.parse::<u64>().map_err(|_| format!("field {field:?} has no count in {rest:?}"))?;
            got.push(name);
        }
        if got != SOURCES {
            return Err(format!(
                "census fields {got:?}, want {SOURCES:?} — the kernel's `Source::NAMES` and \
                 `common::irqcensus::SOURCES` disagree"
            ));
        }
        Ok(())
    })();
    Some(read)
}

/// One CPU's longest interrupts-off and preemption-off windows since the report
/// before, out of that CPU's line of a `mask-windows` kernel's report
/// (`kernel/src/windows.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Windows {
    pub cpu: u32,
    pub irqs_off_ns: u64,
    pub preempt_off_ns: u64,
}

impl Windows {
    /// One line, or why it is not one; anything before `windows: cpu` is ignored.
    pub fn parse(line: &str) -> Option<Result<Self, String>> {
        let rest = line.split("windows: cpu").nth(1)?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let value = |at: usize, name: &str| -> Result<u64, String> {
            fields
                .get(at)
                .and_then(|f| f.strip_prefix(name))
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("no `{name}<ns>` field at {at} in {rest:?}"))
        };
        let parsed = (|| {
            if fields.len() != 3 {
                return Err(format!("{} fields, want 3, in {rest:?}", fields.len()));
            }
            let cpu = fields[0].parse().map_err(|_| format!("unreadable cpu number in {rest:?}"))?;
            Ok(Self { cpu, irqs_off_ns: value(1, "irqs_off_ns=")?, preempt_off_ns: value(2, "preempt_off_ns=")? })
        })();
        Some(parsed)
    }
}

/// The line a `mask-windows` kernel prints once a boot, where it held both of
/// cpuK's windows for a span read off that CPU's counter
/// (`kernel/src/windows.rs`): `windows: held cpuK ns=<span>`.
const HELD: &str = "windows: held cpu";

fn held(line: &str) -> Option<Result<(u32, u64), String>> {
    let rest = line.split(HELD).nth(1)?;
    let parsed = match rest.split_whitespace().collect::<Vec<_>>().as_slice() {
        [cpu, ns] => cpu.parse().ok().zip(ns.strip_prefix("ns=").and_then(|ns| ns.parse().ok())),
        _ => None,
    };
    Some(parsed.ok_or_else(|| format!("unreadable held line {rest:?}")))
}

/// The judge of every `mask-windows` boot, and it judges no duration: every
/// CPU that reported is in every report, and each closed both kinds of window
/// at some point of the boot. Answers each CPU's longest windows over the
/// whole capture.
pub fn windows(capture: &str) -> Result<BTreeMap<u32, Windows>, String> {
    let mut reports: Vec<Windows> = Vec::new();
    for line in capture.lines() {
        if let Some(report) = Windows::parse(line) {
            reports.push(report.map_err(|why| format!("{why}\nline: {line}"))?);
        }
    }
    if reports.is_empty() {
        return Err(format!("no `windows: cpu` report in the capture:\n{capture}"));
    }
    let mut lines: BTreeMap<u32, usize> = BTreeMap::new();
    for report in &reports {
        *lines.entry(report.cpu).or_default() += 1;
    }
    if lines.values().min() != lines.values().max() {
        return Err(format!(
            "windows lines per cpu {lines:?}: a report went out without one of its CPUs"
        ));
    }
    let mut longest: BTreeMap<u32, Windows> = BTreeMap::new();
    for report in &reports {
        let most = longest.entry(report.cpu).or_insert(Windows { irqs_off_ns: 0, preempt_off_ns: 0, ..*report });
        most.irqs_off_ns = most.irqs_off_ns.max(report.irqs_off_ns);
        most.preempt_off_ns = most.preempt_off_ns.max(report.preempt_off_ns);
    }
    for most in longest.values() {
        if most.irqs_off_ns == 0 || most.preempt_off_ns == 0 {
            return Err(format!(
                "cpu{} closed no window of one kind in the whole boot, and every CPU masks and \
                 passes: {most:?}",
                most.cpu
            ));
        }
    }
    Ok(longest)
}

/// What a machine's boot read, each the longest interrupts-off and
/// preemption-off window in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// What the holding CPU reported between its hold and the load's start.
    pub held: (u64, u64),
    /// What any CPU closed in the load's own report.
    pub load: (u64, u64),
}

/// The durations a machine's boot is judged by, read from the reports of
/// `cpus` CPUs before the record `load_exited` heads, the load's exit.
///
/// **The load's reading is its own report alone.** A report carries what
/// closed since the report before, and the load's exit prints one before it
/// tears the process down. So that report spans the teardown of the job
/// before, the runner's spawn of the load and the load up to its exit, and it
/// is refused unless a report before it emptied every CPU's record.
///
/// **A window of known length is read back.** The kernel held both of one
/// CPU's windows for the span its held line states: that CPU's reports from
/// there to the load's start carry a window of each kind no shorter, and none
/// past `timings::ceiling` of it.
pub fn windows_under(capture: &str, cpus: u32, load_exited: &str) -> Result<Measured, String> {
    let mut reports: Vec<(usize, Vec<Windows>)> = Vec::new();
    let mut open: Vec<Windows> = Vec::new();
    let mut hold: Option<(usize, u32, u64)> = None;
    let mut exited: Option<usize> = None;
    for (at, line) in capture.lines().enumerate() {
        if let Some(report) = Windows::parse(line) {
            let report = report.map_err(|why| format!("{why}\nline: {line}"))?;
            if report.cpu as usize != open.len() {
                return Err(format!("a report of {cpus} CPUs names cpu{} at place {}\nline: {line}", report.cpu, open.len()));
            }
            open.push(report);
            if open.len() == cpus as usize {
                reports.push((at, std::mem::take(&mut open)));
            }
        } else if let Some(said) = held(line) {
            let (cpu, ns) = said.map_err(|why| format!("{why}\nline: {line}"))?;
            if hold.replace((at, cpu, ns)).is_some() {
                return Err(format!("a second hold, and the kernel holds once a boot\nline: {line}"));
            }
        } else if exited.is_none() && line.contains(load_exited) {
            exited = Some(at);
        }
    }
    let exited = exited.ok_or_else(|| format!("no `{load_exited}` record: the load never ended"))?;
    let own = reports
        .iter()
        .rposition(|(at, _)| *at < exited)
        .ok_or_else(|| format!("no report before `{load_exited}`: the load's exit printed none"))?;
    if own == 0 {
        return Err("the load's report is the boot's first: none was taken as the load started, so it \
                    carries every window since each CPU joined"
            .to_string());
    }
    let longest = |lines: &mut dyn Iterator<Item = &Windows>| {
        lines.fold((0, 0), |(irqs, preempt), w| (irqs.max(w.irqs_off_ns), preempt.max(w.preempt_off_ns)))
    };

    let (hold_at, cpu, ns) =
        hold.ok_or_else(|| format!("no `{HELD}` line: this kernel held no window of known length"))?;
    if ns < kernel::sched::windows::HELD_NS {
        return Err(format!("cpu{cpu} held its windows for {ns} ns, and the kernel owes {}", kernel::sched::windows::HELD_NS));
    }
    let mut read_back = reports[..own]
        .iter()
        .filter(|(at, _)| *at > hold_at)
        .filter_map(|(_, report)| report.get(cpu as usize))
        .peekable();
    if read_back.peek().is_none() {
        return Err(format!(
            "cpu{cpu} reported nothing between its hold and the load's own report, so the load's \
             reading carries the hold"
        ));
    }
    let held = longest(&mut read_back);
    let read = format!(
        "cpu{cpu} held both windows for {ns} ns, and its reports before the load's read back \
         irqs_off_ns={} preempt_off_ns={}",
        held.0, held.1
    );
    if held.0 < ns || held.1 < ns {
        return Err(format!("{read}: a window was reported shorter than it was held"));
    }
    let ceiling = toyos_metal::timings::ceiling(ns);
    if held.0 > ceiling || held.1 > ceiling {
        return Err(format!(
            "{read}, past the ceiling of {ceiling}: a window was reported at more than twice what was held"
        ));
    }
    let load = longest(&mut reports[own].1.iter());
    Ok(Measured { held, load })
}
