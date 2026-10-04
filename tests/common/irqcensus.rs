//! The kernel's interrupt census, and the windows a `mask-windows` kernel
//! prints beside it, read back on the host.
//!
//! The guest prints `irq: cpuN total=… timer=… …` per online CPU whenever a
//! process exits, on `SYS_SHUTDOWN` and on the blocked-task dump
//! (`kernel/src/irq_census.rs`). The counters are cumulative since boot, so the
//! **last** line a capture holds for a CPU is that boot's whole census.
//!
//! Two readers, and they are why this is a module rather than a closure:
//! `irq_census_conservation` asks whether one boot's census is internally
//! consistent, and the suite's own summary asks where the machine's interrupts
//! landed across every guest a run booted. The second is the instrument the
//! `every-interrupt-lands-on-the-boot-cpu` track's later change is measured
//! against, so it has to be produced by an ordinary run rather than by
//! `--nocapture`: a number only a developer's terminal can produce is not a
//! baseline.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// The census's source names, in the order `kernel/src/irq_census.rs` prints
/// them. The kernel's `Source::NAMES` is the definition; this is the host's copy
/// and [`Census::parse`] refuses a line whose fields are not exactly these, so
/// a source added on one side and not the other is a red rather than a silently
/// dropped column.
pub const SOURCES: [&str; 12] = [
    "timer", "kick", "xhci", "userdev", "sound", "i8042", "dmafault", "hda", "tlb", "nmi", "spurious",
    "unclaimed",
];

/// The sources whose delivery CPU is chosen by the interrupt controller rather
/// than by the CPU that took the work — every device vector, in other words.
/// `MSG_ADDR` names physical destination 0 and the one I/O APIC pin this kernel
/// routes goes to the BSP, so today every one of these is cpu0's alone. The day
/// that stops being true is the day the track's change lands.
pub const DEVICE_SOURCES: [&str; 6] = ["xhci", "userdev", "sound", "i8042", "dmafault", "hda"];

/// One CPU's counters out of one `irq:` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Census {
    pub cpu: u32,
    pub total: u64,
    /// Indexed the same as [`SOURCES`].
    pub by_source: [u64; SOURCES.len()],
}

impl Census {
    /// Parse one line, or say why it is not one.
    ///
    /// Anything before `irq: cpu` is ignored, so the same parser reads a raw
    /// guest line, a kernel record's line and a `[serial N]` echo of either.
    pub fn parse(line: &str) -> Option<Result<Self, String>> {
        let rest = line.split("irq: cpu").nth(1)?;
        Some(Self::parse_body(rest))
    }

    fn parse_body(rest: &str) -> Result<Self, String> {
        let mut fields = rest.split_whitespace();
        let cpu: u32 = fields
            .next()
            .ok_or_else(|| format!("no cpu number in {rest:?}"))?
            .parse()
            .map_err(|_| format!("unreadable cpu number in {rest:?}"))?;
        let mut named = Vec::new();
        for field in fields {
            let (name, value) = field
                .split_once('=')
                .ok_or_else(|| format!("field {field:?} is not name=value in {rest:?}"))?;
            let value: u64 = value
                .parse()
                .map_err(|_| format!("field {field:?} has no count in {rest:?}"))?;
            named.push((name, value));
        }
        let want: Vec<&str> = std::iter::once("total").chain(SOURCES).collect();
        let got: Vec<&str> = named.iter().map(|(n, _)| *n).collect();
        if got != want {
            return Err(format!(
                "census fields {got:?}, want {want:?} — the kernel's `Source::NAMES` and \
                 `common::irqcensus::SOURCES` disagree"
            ));
        }
        let mut by_source = [0u64; SOURCES.len()];
        for (slot, (_, value)) in by_source.iter_mut().zip(&named[1..]) {
            *slot = *value;
        }
        Ok(Self { cpu, total: named[0].1, by_source })
    }

    pub fn source(&self, name: &str) -> u64 {
        let i = SOURCES.iter().position(|s| *s == name).expect("no such census source");
        self.by_source[i]
    }

    /// Every source summed. Equal to [`Self::total`] on a census that adds up —
    /// which is the whole of what `irq_census_conservation` asks, because the
    /// kernel counts the total apart from the sources rather than deriving it.
    pub fn sum_of_sources(&self) -> u64 {
        self.by_source.iter().sum()
    }
}

/// One CPU's longest interrupts-off and preemption-off windows since the report
/// before, out of the line a `mask-windows` kernel prints beside that CPU's
/// census (`kernel/src/windows.rs`).
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

/// The judge of every `mask-windows` boot, and it judges no duration: each
/// census line has its CPU's windows line beside it, and each CPU closed both
/// kinds of window at some point of the boot. Answers each CPU's longest
/// windows over the whole capture.
pub fn windows(capture: &str) -> Result<BTreeMap<u32, Windows>, String> {
    let mut censuses: BTreeMap<u32, usize> = BTreeMap::new();
    let mut reports: Vec<Windows> = Vec::new();
    for line in capture.lines() {
        if let Some(census) = Census::parse(line) {
            *censuses.entry(census.map_err(|why| format!("{why}\nline: {line}"))?.cpu).or_default() += 1;
        } else if let Some(report) = Windows::parse(line) {
            reports.push(report.map_err(|why| format!("{why}\nline: {line}"))?);
        }
    }
    if censuses.is_empty() {
        return Err(format!("no `irq: cpu` census in the capture:\n{capture}"));
    }
    let mut beside: BTreeMap<u32, usize> = BTreeMap::new();
    for report in &reports {
        *beside.entry(report.cpu).or_default() += 1;
    }
    if beside != censuses {
        return Err(format!(
            "census lines per cpu {censuses:?} and windows lines per cpu {beside:?}: a census \
             went out without its windows, or windows without their census"
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
/// past `metaltimings::ceiling` of it.
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
    let ceiling = toyos_build::metaltimings::ceiling(ns);
    if held.0 > ceiling || held.1 > ceiling {
        return Err(format!(
            "{read}, past the ceiling of {ceiling}: a window was reported at more than twice what was held"
        ));
    }
    let load = longest(&mut reports[own].1.iter());
    Ok(Measured { held, load })
}

/// The newest census each CPU of each guest printed, keyed by the boot's own
/// sequence number.
///
/// Newest wins because the counters are cumulative: the last line a guest wrote
/// is its whole boot. A guest that printed none — one that booted and ran no
/// program — contributes nothing and is counted as such in the summary, which is
/// the honest form: it did take interrupts, and no capture says how many.
static SEEN: Mutex<BTreeMap<u32, BTreeMap<u32, Census>>> = Mutex::new(BTreeMap::new());

/// Offer one console line to the census. Called from every boot's reader thread,
/// on every line, so it does the cheapest possible thing first.
pub fn observe(seq: u32, line: &str) {
    if !line.contains("irq: cpu") {
        return;
    }
    let Some(Ok(census)) = Census::parse(line) else {
        return;
    };
    let mut seen = SEEN.lock().expect("census map poisoned");
    seen.entry(seq).or_default().insert(census.cpu, census);
}

/// One guest's whole census.
struct Guest {
    /// Interrupts on cpu0 as a fraction of the machine's.
    boot_cpu_share: f64,
    /// Interrupts on cpu0, so the run's pooled share is an exact ratio of two
    /// integers rather than a mean of per-guest fractions.
    on_boot_cpu: u64,
    total: u64,
    /// Per source, summed over every CPU, and the cpu0 part of it.
    per_source: [(u64, u64); SOURCES.len()],
    cpus: usize,
}

fn guests() -> Vec<Guest> {
    let seen = SEEN.lock().expect("census map poisoned");
    seen.values()
        .filter_map(|by_cpu| {
            let total: u64 = by_cpu.values().map(|c| c.total).sum();
            if total == 0 {
                return None;
            }
            let boot = by_cpu.get(&0).map_or(0, |c| c.total);
            let mut per_source = [(0u64, 0u64); SOURCES.len()];
            for census in by_cpu.values() {
                for (slot, count) in per_source.iter_mut().zip(census.by_source) {
                    slot.0 += count;
                    if census.cpu == 0 {
                        slot.1 += count;
                    }
                }
            }
            Some(Guest {
                boot_cpu_share: boot as f64 / total as f64,
                on_boot_cpu: boot,
                total,
                per_source,
                cpus: by_cpu.len(),
            })
        })
        .collect()
}

/// The order statistic at `q` of an already-sorted sample, nearest-rank.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((sorted.len() as f64) * q).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// What this run saw, as the suite's last word before its tally.
///
/// Empty when no guest printed a census — a filtered run of boot-only tests is
/// exactly that, and a summary claiming a distribution it never measured would
/// be worse than none.
pub fn summary() -> String {
    use std::fmt::Write;
    let guests = guests();
    if guests.is_empty() {
        return String::new();
    }
    let mut shares: Vec<f64> = guests.iter().map(|g| g.boot_cpu_share).collect();
    shares.sort_by(|a, b| a.partial_cmp(b).expect("a share is never NaN"));
    let total: u64 = guests.iter().map(|g| g.total).sum();
    let mut out = String::new();
    let on_boot_cpu: u64 = guests.iter().map(|g| g.on_boot_cpu).sum();
    let _ = writeln!(
        out,
        "  --- irq census: {} guest(s) reported, {} interrupt(s), {on_boot_cpu} of them on cpu0 \
         ({:.1}%); per guest cpu0's share is median {:.1}% p90 {:.1}% max {:.1}%",
        guests.len(),
        total,
        on_boot_cpu as f64 / total as f64 * 100.0,
        quantile(&shares, 0.5) * 100.0,
        quantile(&shares, 0.9) * 100.0,
        shares[shares.len() - 1] * 100.0,
    );
    for (i, name) in SOURCES.iter().enumerate() {
        let all: u64 = guests.iter().map(|g| g.per_source[i].0).sum();
        if all == 0 {
            continue;
        }
        let on_boot_cpu: u64 = guests.iter().map(|g| g.per_source[i].1).sum();
        let _ = writeln!(
            out,
            "      {name:<9} {all:>9} ({:.1}% of all), {:.1}% of them on cpu0",
            all as f64 / total as f64 * 100.0,
            on_boot_cpu as f64 / all as f64 * 100.0,
        );
    }
    let widest = guests.iter().map(|g| g.cpus).max().unwrap_or(0);
    let _ = writeln!(out, "      widest guest reported {widest} cpu(s)");
    out
}
