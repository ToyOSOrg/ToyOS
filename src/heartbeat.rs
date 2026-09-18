//! Whether a heartbeat capture settled and what its mask then says — the
//! verdict `kernel_heartbeat` in `tests/toyos.rs` reads. Text in, a verdict
//! out, so a capture the instrument has already taken replays here against the
//! rule.
//!
//! **What a clear bit can be evidence of.** `mask=` says which CPUs reached a
//! scheduler pass in the period (`kernel/src/heartbeat.rs`), and a CPU running
//! one task with nothing to preempt it takes its timer and reaches none, as does
//! one inside a disk wait, which cannot park — so a clear bit reads "busy" and
//! "stopped" alike. The test exists for the second, on a machine that is
//! otherwise running, and a beat can say so only where the machine held that
//! state:
//!
//! - **after the boot's start-up.** That is not `Boot: complete`, which is
//!   printed as `init` is spawned, and not `init`'s last spawn record either:
//!   the programs `init` starts do their own start-up after it, and that work
//!   is what keeps a CPU off the mask. The boot has started when every
//!   `[boot] start` program has said it is done — its ready line or its exit
//!   record, which `DONE` is the one table of — and the window opens at the
//!   first beat whose whole period follows the last of them.
//! - **on a beat the machine ran through.** A beat later than `LATE` periods
//!   is a period no CPU reached the idle loop in, and `ran=0` is a period no
//!   CPU dispatched a task in. Either is the machine not running — a guest its
//!   host did not schedule — and what a CPU did in it is unreadable, so such a
//!   beat closes the window and the sample is [`Refused::NotRunning`], never a
//!   CPU missing.
//! - **over `MIN_SETTLED` beats.** A verdict about a CPU is read from that many
//!   settled beats or from none: a shorter window says only why it is short.
//!
//! Inside the window a CPU absent from [`STOPPED_BEATS`] consecutive beats has
//! stopped: `diag-tick` caps a sleep at 100 ms against a 250 ms line, so it has
//! missed five wakes. Absent from one and back on the next it has missed two,
//! which the owning instrument produces on a healthy guest.
//!
//! **The capture follows the window, not a clock.** How long a boot's start-up
//! takes is the loaded host's to decide, so an instrument that drains for a
//! fixed span hands this module whatever is left over and reds on its own
//! refusal when that is less than a verdict needs. [`window_beats`] is what a
//! capture is taken to: it says how much window the capture holds so far, and
//! [`CAPTURE_BEATS`] is enough.

#![forbid(unsafe_code)]

/// The line's period, `kernel/src/heartbeat.rs`'s `PERIOD_NS`.
pub const PERIOD_MS: u64 = 250;

/// A beat whose `gap=` exceeds this many periods is one the machine did not run
/// through: eight CPUs whose longest sleep is 100 ms, and none reached the idle
/// loop for a whole period.
const LATE: u64 = 2;

/// Consecutive settled beats a CPU is absent from before it has stopped.
pub const STOPPED_BEATS: usize = 2;

/// The fewest settled beats a verdict is read from.
const MIN_SETTLED: usize = 4;

/// Settled beats a capture is taken to. `MIN_SETTLED` is the floor a verdict is
/// read from and the spare is detection, not slack: a CPU whose first absence
/// is the capture's last beat is a blip and convicts nobody, so the capture
/// carries beats past the floor for the [`STOPPED_BEATS`]th one to land in.
pub const CAPTURE_BEATS: usize = MIN_SETTLED + 3;

/// The widest `alive=N/M` denominator that is a reading of the `mask=` beside
/// it: that mask is 64 bits, so no `M` at 64 or above describes it, and `M = 0`
/// describes no machine.
const MOST_CPUS: u32 = 63;

/// One `heartbeat: t=… alive=… mask=… ran=… gap=…` line, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beat {
    /// Index of the line in the capture.
    pub line: usize,
    pub t_ms: u64,
    pub cpus: u32,
    pub mask: u64,
    pub ran: u64,
    pub gap_ms: u64,
}

impl Beat {
    /// The beat's reading of `text`, or `None` where any field is unreadable.
    fn parse(line: usize, text: &str) -> Option<Beat> {
        let field = |key: &str| text.split(key).nth(1)?.split_whitespace().next();
        let cpus: u32 = field("alive=")?.split_once('/')?.1.parse().ok()?;
        if !(1..=MOST_CPUS).contains(&cpus) {
            return None;
        }
        Some(Beat {
            line,
            t_ms: millis(field("t=")?)?,
            cpus,
            mask: u64::from_str_radix(field("mask=0x")?, 16).ok()?,
            ran: field("ran=")?.parse().ok()?,
            gap_ms: millis(field("gap=")?)?,
        })
    }

    fn full(&self) -> bool {
        self.mask == (1u64 << self.cpus) - 1
    }

    fn absent(&self, cpu: u32) -> bool {
        self.mask & (1 << cpu) == 0
    }

    /// Whether the machine ran through this beat's period.
    fn ran_through(&self) -> bool {
        self.gap_ms <= LATE * PERIOD_MS && self.ran > 0
    }
}

/// `S.mmms` as milliseconds.
fn millis(field: &str) -> Option<u64> {
    let (s, ms) = field.strip_suffix('s')?.split_once('.')?;
    if ms.len() != 3 {
        return None;
    }
    Some(s.parse::<u64>().ok()? * 1000 + ms.parse::<u64>().ok()?)
}

/// `tests/metalcase`'s `[boot] start` programs and the line each says it has
/// finished starting with. The one table: [`done_lines`] holds it against the
/// config, and a caller reads it through that rather than declaring its own.
const DONE: &[(&str, &str)] = &[
    ("logd", "logd: this boot's kernel log is"),
    ("compositor", "compositor: ready"),
    ("soundd", "soundd: null sink idle"),
    ("netd", "exit: netd pid="),
    ("sshd", "exit: sshd pid="),
    ("test-runner", "===READY==="),
];

/// The done line of each program in `start`, or the disagreement between the
/// config and [`DONE`] — a `[boot] start` program with no done line here leaves
/// its own start-up inside the window, which is the one thing the window exists
/// to exclude.
pub fn done_lines(start: &[String]) -> Result<Vec<&'static str>, String> {
    let start: Vec<&str> = start.iter().map(String::as_str).collect();
    let known: Vec<&str> = DONE.iter().map(|(program, _)| *program).collect();
    if start != known {
        return Err(format!(
            "`tests/metalcase` starts {start:?} and `src/heartbeat.rs` knows the done line of \
             {known:?} — a program without one leaves its start-up inside the window"
        ));
    }
    Ok(DONE.iter().map(|(_, line)| *line).collect())
}

/// Why a capture is not a claim about a settled, running machine's CPUs.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// A `heartbeat: t=` line one of whose fields would not parse.
    Unreadable(String),
    /// A `[boot] start` program never said it was done: the line it says it with.
    BootUnfinished(String),
    /// Fewer than `MIN_SETTLED` beats had a whole period after the boot's
    /// start-up.
    Unsettled { settled: usize, beats: usize },
    /// A settled beat the machine did not run through, after `held` it did.
    NotRunning { beat: Beat, held: usize },
    /// CPUs absent from [`STOPPED_BEATS`] consecutive settled beats, of `settled`
    /// from the capture line `opened`.
    CpuMissing { cpus: Vec<u32>, settled: usize, opened: usize },
}

/// The settled window, read.
#[derive(Debug, PartialEq, Eq)]
pub struct Settled {
    pub beats: Vec<Beat>,
    /// Settled beats missing a CPU — for one line each, since none for two.
    pub blips: usize,
    /// The widest `gap=` anywhere in the capture, settled or not: a window
    /// between two lines is wide enough to hide a death wherever it falls.
    pub widest_gap_ms: u64,
}

/// The beats whose whole period follows the last of `started`, or the done line
/// nothing in `lines` said.
fn window<'a>(beats: &'a [Beat], lines: &[&str], started: &[&str]) -> Result<&'a [Beat], String> {
    let mut last = 0;
    for said in started {
        let Some(at) = lines.iter().position(|l| l.contains(said)) else {
            return Err((*said).to_string());
        };
        last = last.max(at);
    }
    // The beat after the record straddles it; the one after that is the first
    // whose whole period follows it.
    let after = beats.iter().filter(|b| b.line > last).count();
    Ok(&beats[(beats.len() - after + 1).min(beats.len())..])
}

/// How much window `lines` holds so far — what a capture is taken to, against
/// [`CAPTURE_BEATS`]. Zero until every one of `started` has said it is done, and
/// a line whose fields will not parse is not a beat here; [`settle`] is what
/// refuses one.
pub fn window_beats(lines: &[&str], started: &[&str]) -> usize {
    let beats: Vec<Beat> =
        lines.iter().enumerate().filter_map(|(i, l)| Beat::parse(i, l)).collect();
    window(&beats, lines, started).map_or(0, <[Beat]>::len)
}

/// The verdict on `lines`, a capture whose boot's start-up ends with the last of
/// `started` — one line per `[boot] start` program, the one it says it is done
/// with.
pub fn settle(lines: &[&str], started: &[&str]) -> Result<Settled, Refused> {
    let beats = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("heartbeat: t="))
        .map(|(i, l)| Beat::parse(i, l).ok_or_else(|| Refused::Unreadable(l.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(odd) = beats.iter().find(|b| b.cpus != beats[0].cpus) {
        return Err(Refused::Unreadable(lines[odd.line].to_string()));
    }
    let window = window(&beats, lines, started).map_err(Refused::BootUnfinished)?;
    let held = window.iter().position(|b| !b.ran_through()).unwrap_or(window.len());
    let read = &window[..held];
    // A CPU is read from `MIN_SETTLED` settled beats or from none, so what a
    // short window says is only why it is short.
    if held < MIN_SETTLED {
        return Err(if held < window.len() {
            Refused::NotRunning { beat: window[held].clone(), held }
        } else {
            Refused::Unsettled { settled: held, beats: beats.len() }
        });
    }
    let cpus: Vec<u32> = (0..beats[0].cpus)
        .filter(|&c| read.windows(STOPPED_BEATS).any(|w| w.iter().all(|b| b.absent(c))))
        .collect();
    if !cpus.is_empty() {
        return Err(Refused::CpuMissing { cpus, settled: read.len(), opened: read[0].line });
    }
    if held < window.len() {
        return Err(Refused::NotRunning { beat: window[held].clone(), held });
    }
    Ok(Settled {
        blips: read.iter().filter(|b| !b.full()).count(),
        widest_gap_ms: beats.iter().map(|b| b.gap_ms).max().unwrap_or(0),
        beats: read.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// `tests/metalcase`'s done lines, as the test passes them.
    fn started() -> Vec<&'static str> {
        done_lines(&crate::build::boot_start(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase/system.toml"),
        ))
        .unwrap()
    }

    /// Nightly `35072262489`, guest shard 8, suite run: every heartbeat line and
    /// every `[boot] start` program's done line, in the capture's order and
    /// verbatim, the lines between them dropped.
    const SHARD_8_SUITE: &str = "\
logd: this boot's kernel log is /log/2026-09-16-083530.log (2026-09-16 08:35:30 at UTC+0 recovered from two readings)
[kernel 1.109 cpu1] heartbeat: t=1.109s alive=8/8 mask=0xff ran=11 gap=0.260s
[kernel 1.361 cpu7] heartbeat: t=1.361s alive=7/8 mask=0xdf ran=4 gap=0.251s
[kernel 1.361 cpu7] heartbeat: cpu5 last reached one 0.307s ago
[kernel 1.618 cpu3] heartbeat: t=1.618s alive=7/8 mask=0xbf ran=5 gap=0.257s
[kernel 1.618 cpu3] heartbeat: cpu6 last reached one 0.423s ago
soundd: null sink idle
[kernel 1.762 cpu7] exit: netd pid=7 code=0 cpu=399ms
[kernel 1.934 cpu1] heartbeat: t=1.934s alive=7/8 mask=0xfe ran=9 gap=0.315s
[kernel 1.934 cpu1] heartbeat: cpu0 last reached one 0.572s ago
===READY===
[kernel 2.021 cpu0] spawn: /system/bin/test-runner pid=9 tid=0 dst=2 base=0x10000000000 entry=0x1000001ff80 cr3=0x6ecf000 symbols=2048KiB (layout=21ms relocs=0ms deps=0ms tls=1ms total=75ms)
[kernel 2.184 cpu0] heartbeat: t=2.184s alive=7/8 mask=0xdf ran=8 gap=0.250s
[kernel 2.184 cpu0] heartbeat: cpu5 last reached one 0.339s ago
[kernel 2.434 cpu0] heartbeat: t=2.434s alive=7/8 mask=0xdf ran=15 gap=0.250s
[kernel 2.434 cpu0] heartbeat: cpu5 last reached one 0.589s ago
compositor: ready
[kernel 2.618 cpu1] exit: sshd pid=8 code=0 cpu=634ms
[kernel 2.686 cpu2] heartbeat: t=2.686s alive=8/8 mask=0xff ran=36 gap=0.251s
[kernel 2.937 cpu2] heartbeat: t=2.937s alive=8/8 mask=0xff ran=43 gap=0.251s
[kernel 3.188 cpu2] heartbeat: t=3.188s alive=8/8 mask=0xff ran=43 gap=0.251s
[kernel 3.439 cpu2] heartbeat: t=3.439s alive=8/8 mask=0xff ran=43 gap=0.251s
[kernel 3.690 cpu5] heartbeat: t=3.690s alive=8/8 mask=0xff ran=42 gap=0.250s
[kernel 3.940 cpu2] heartbeat: t=3.940s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 4.196 cpu5] heartbeat: t=4.196s alive=8/8 mask=0xff ran=44 gap=0.255s
[kernel 4.449 cpu5] heartbeat: t=4.449s alive=8/8 mask=0xff ran=44 gap=0.252s
[kernel 4.703 cpu2] heartbeat: t=4.703s alive=8/8 mask=0xff ran=43 gap=0.253s
[kernel 4.953 cpu2] heartbeat: t=4.953s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 5.203 cpu2] heartbeat: t=5.203s alive=8/8 mask=0xff ran=44 gap=0.250s
";

    /// The same shard's ALONE re-run, the same way.
    const SHARD_8_ALONE: &str = "\
logd: this boot's kernel log is /log/2026-09-16-083541.log (2026-09-16 08:35:41 at UTC+0 recovered from two readings)
[kernel 1.029 cpu1] heartbeat: t=1.029s alive=8/8 mask=0xff ran=11 gap=0.262s
[kernel 1.290 cpu7] heartbeat: t=1.290s alive=7/8 mask=0xdf ran=4 gap=0.260s
[kernel 1.290 cpu7] heartbeat: cpu5 last reached one 0.323s ago
[kernel 1.541 cpu3] heartbeat: t=1.541s alive=7/8 mask=0xbf ran=5 gap=0.251s
[kernel 1.544 cpu3] heartbeat: cpu6 last reached one 0.429s ago
soundd: null sink idle
[kernel 1.692 cpu7] exit: netd pid=7 code=0 cpu=402ms
[kernel 1.793 cpu7] heartbeat: t=1.793s alive=7/8 mask=0xfe ran=10 gap=0.251s
[kernel 1.793 cpu7] heartbeat: cpu0 last reached one 0.499s ago
===READY===
[kernel 1.951 cpu0] spawn: /system/bin/test-runner pid=9 tid=0 dst=2 base=0x10000000000 entry=0x1000001ff80 cr3=0x2d83000 symbols=2048KiB (layout=9ms relocs=0ms deps=0ms tls=1ms total=79ms)
[kernel 2.043 cpu0] heartbeat: t=2.043s alive=7/8 mask=0xdf ran=8 gap=0.250s
[kernel 2.043 cpu0] heartbeat: cpu5 last reached one 0.281s ago
[kernel 2.293 cpu0] heartbeat: t=2.293s alive=6/8 mask=0xdd ran=1 gap=0.250s
[kernel 2.293 cpu0] heartbeat: cpu1 last reached one 0.422s ago
[kernel 2.293 cpu0] heartbeat: cpu5 last reached one 0.531s ago
compositor: ready
[kernel 2.543 cpu0] heartbeat: t=2.543s alive=7/8 mask=0xdf ran=25 gap=0.250s
[kernel 2.543 cpu0] heartbeat: cpu5 last reached one 0.781s ago
[kernel 2.624 cpu1] exit: sshd pid=8 code=0 cpu=708ms
[kernel 2.793 cpu5] heartbeat: t=2.793s alive=8/8 mask=0xff ran=42 gap=0.250s
[kernel 3.044 cpu2] heartbeat: t=3.044s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 3.294 cpu2] heartbeat: t=3.294s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 3.545 cpu2] heartbeat: t=3.545s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 3.796 cpu2] heartbeat: t=3.796s alive=8/8 mask=0xff ran=43 gap=0.251s
[kernel 4.047 cpu2] heartbeat: t=4.047s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 4.298 cpu2] heartbeat: t=4.298s alive=8/8 mask=0xff ran=43 gap=0.250s
[kernel 4.557 cpu2] heartbeat: t=4.557s alive=8/8 mask=0xff ran=44 gap=0.259s
[kernel 4.814 cpu2] heartbeat: t=4.814s alive=8/8 mask=0xff ran=43 gap=0.256s
[kernel 5.065 cpu2] heartbeat: t=5.065s alive=8/8 mask=0xff ran=43 gap=0.251s
";

    /// A dev-host boot, up to its torn last beat: every heartbeat line, every
    /// `[boot] start` program's done line and the two lines naming the wait, in
    /// the capture's order and verbatim, the lines between them dropped.
    const DISK_WAIT_PINS_CPU4: &str = "\
logd: this boot's kernel log is /log/2026-09-18-135738.log (2026-09-18 13:57:38 at UTC+0 recovered from two readings)
[kernel 1.122 cpu3] heartbeat: t=1.121s alive=8/8 mask=0xff ran=11 gap=0.261s
[kernel 1.395 cpu1] heartbeat: t=1.395s alive=8/8 mask=0xff ran=4 gap=0.273s
soundd: null sink idle
[kernel 1.529 cpu7] exit: netd pid=7 code=0 cpu=94ms
[kernel 1.645 cpu0] heartbeat: t=1.645s alive=8/8 mask=0xff ran=22 gap=0.250s
===READY===
[kernel 1.895 cpu0] heartbeat: t=1.895s alive=6/8 mask=0xdb ran=4 gap=0.250s
[kernel 2.146 cpu0] heartbeat: t=2.145s alive=6/8 mask=0xdd ran=23 gap=0.250s
[kernel 2.230 cpu1] exit: sshd pid=8 code=0 cpu=666ms
[kernel 2.397 cpu2] heartbeat: t=2.397s alive=8/8 mask=0xff ran=31 gap=0.251s
compositor: ready
[kernel 2.658 cpu2] heartbeat: t=2.658s alive=8/8 mask=0xff ran=33 gap=0.261s
[kernel 2.909 cpu0] heartbeat: t=2.909s alive=8/8 mask=0xff ran=38 gap=0.250s
[kernel 3.159 cpu0] heartbeat: t=3.159s alive=7/8 mask=0xef ran=35 gap=0.250s
[kernel 3.409 cpu0] heartbeat: t=3.409s alive=7/8 mask=0xef ran=26 gap=0.250s
[kernel 3.659 cpu0] heartbeat: t=3.659s alive=7/8 mask=0xef ran=38 gap=0.250s
[kernel 3.909 cpu0] heartbeat: t=3.909s alive=7/8 mask=0xef ran=32 gap=0.250s
[kernel 4.159 cpu0] heartbeat: t=4.159s alive=7/8 mask=0xef ran=37 gap=0.250s
[kernel 4.409 cpu0] heartbeat: t=4.409s alive=7/8 mask=0xef ran=37 gap=0.250s
[kernel 4.659 cpu0] heartbeat: t=4.659s alive=7/8 mask=0xef ran=33 gap=0.250s
[kernel 4.663 cpu4] usb-storage: 00:02.0 slot 1 transport broke on SCSI 0x2a: no answer in the status phase in 2000 ms
[kernel 4.677 cpu4] fsync: /log/2026-09-18-135738.log durable on attempt 2 after 2016ms — a refused attempt kept every page dirty and a later one delivered them
";

    /// A CPU a disk wait pins reaches no pass, and that is what the mask says:
    /// every beat of it ran through, so neither refusal excuses it, and the
    /// verdict names the CPU. The red's owner is the wait that cannot park.
    #[test]
    fn a_cpu_pinned_by_a_disk_wait_is_reported_and_not_excused() {
        let lines = lines(DISK_WAIT_PINS_CPU4);
        let opened = lines.iter().position(|l| l.contains("t=2.909s")).unwrap();
        assert_eq!(
            settle(&lines, &started()),
            Err(Refused::CpuMissing { cpus: vec![4], settled: 8, opened })
        );
    }

    fn lines(capture: &str) -> Vec<&str> {
        capture.lines().collect()
    }

    fn beat(t_ms: u64, mask: u64, ran: u64, gap_ms: u64) -> String {
        format!(
            "[kernel {}.{:03} cpu2] heartbeat: t={}.{:03}s alive={}/8 mask={mask:#04x} ran={ran} \
             gap={}.{:03}s",
            t_ms / 1000,
            t_ms % 1000,
            t_ms / 1000,
            t_ms % 1000,
            mask.count_ones(),
            gap_ms / 1000,
            gap_ms % 1000,
        )
    }

    /// A capture whose boot's start-up ends before its first beat, then `beats`.
    fn settled_capture(beats: &[String]) -> String {
        let mut capture: String = started().iter().map(|s| format!("{s}\n")).collect();
        capture.push_str(&beat(2000, 0xff, 40, 250));
        capture.push('\n');
        for b in beats {
            capture.push_str(b);
            capture.push('\n');
        }
        capture
    }

    /// The started programs' own start-up outlasts `init`'s last spawn record,
    /// so a window opened at a full mask holds cpu5 absent from two consecutive
    /// beats; opened after the last done line it holds no clear bit at all.
    #[test]
    fn the_nightly_shard_8_boots_settle_after_the_last_program_finishes_starting() {
        for (capture, opens_at, settled, widest) in
            [(SHARD_8_SUITE, 2937, 10, 315), (SHARD_8_ALONE, 3044, 9, 262)]
        {
            let lines = lines(capture);
            let beats: Vec<Beat> = lines
                .iter()
                .enumerate()
                .filter_map(|(i, l)| Beat::parse(i, l))
                .collect();
            assert_eq!(beats.len(), 17);
            assert!(beats[0].full());
            let spawned = lines
                .iter()
                .position(|l| l.contains("spawn: /system/bin/test-runner"))
                .unwrap();
            let after_spawn: Vec<&Beat> = beats.iter().filter(|b| b.line > spawned).collect();
            assert!(after_spawn[0].absent(5) && after_spawn[1].absent(5));
            let verdict = settle(&lines, &started()).unwrap();
            assert_eq!(verdict.beats[0].t_ms, opens_at);
            assert_eq!(verdict.beats.len(), settled);
            assert_eq!(verdict.blips, 0);
            assert_eq!(verdict.widest_gap_ms, widest);
            let exit = lines.iter().position(|l| l.contains("exit: sshd")).unwrap();
            let straddling = beats.iter().find(|b| b.line > exit).unwrap();
            assert!(straddling.t_ms < opens_at);
        }
    }

    /// A dev-host boot the host stopped scheduling: 37 beats, a pair of full
    /// masks at t=2.654 s and 2.904 s, then seven seconds of `alive=4/8 ran=0`
    /// naming cpu[0, 1, 2, 4, 5]. Reconstructed from those numbers rather than
    /// captured — the boot beats and the split of the missing set across the
    /// tail are this test's.
    fn dev_host_capture() -> String {
        let started = started();
        let mut capture = String::new();
        for said in &started[..4] {
            capture.push_str(said);
            capture.push('\n');
        }
        for (t, mask, ran) in [(904, 0xff, 11), (1154, 0xdf, 4), (1404, 0xbf, 5), (1654, 0xfe, 9)] {
            capture.push_str(&beat(t, mask, ran, 250));
            capture.push('\n');
        }
        capture.push_str("===READY===\n");
        for (t, mask, ran) in [(1904, 0xdf, 8), (2154, 0xdf, 15)] {
            capture.push_str(&beat(t, mask, ran, 250));
            capture.push('\n');
        }
        capture.push_str("[kernel 2.3 cpu1] exit: sshd pid=8 code=0 cpu=634ms\n");
        for (t, mask, ran) in [(2404, 0xdd, 1), (2654, 0xff, 36), (2904, 0xff, 43)] {
            capture.push_str(&beat(t, mask, ran, 250));
            capture.push('\n');
        }
        for i in 0..28u64 {
            let mask = if i < 14 { 0xe8 } else { 0xc9 };
            capture.push_str(&beat(3154 + i * 250, mask, 0, 250));
            capture.push('\n');
        }
        capture
    }

    #[test]
    fn the_dev_host_capture_is_a_machine_that_was_not_running() {
        let capture = dev_host_capture();
        let lines = lines(&capture);
        let beats: Vec<Beat> =
            lines.iter().enumerate().filter_map(|(i, l)| Beat::parse(i, l)).collect();
        assert_eq!(beats.len(), 37);
        let pair = beats.windows(2).position(|w| w[0].full() && w[1].full()).unwrap();
        assert_eq!(beats[pair].t_ms, 2654);
        let stopped: Vec<u32> = (0..8)
            .filter(|&c| beats[pair..].windows(2).any(|w| w[0].absent(c) && w[1].absent(c)))
            .collect();
        assert_eq!(stopped, [0, 1, 2, 4, 5]);
        assert_eq!(
            settle(&lines, &started()),
            Err(Refused::NotRunning { beat: beats[pair + 2].clone(), held: 2 })
        );
        assert_eq!(beats[pair + 2].ran, 0);
    }

    /// The owning instrument's healthy shape: one line a CPU is absent from and
    /// back on is two missed wakes, not a stopped CPU.
    #[test]
    fn one_line_absent_and_back_is_a_blip() {
        let mut beats: Vec<String> = (1..=9).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
        beats.push(beat(4500, 0xbf, 43, 250));
        beats.extend((1..=8).map(|i| beat(4500 + i * 250, 0xff, 43, 250)));
        let capture = settled_capture(&beats);
        let verdict = settle(&lines(&capture), &started()).unwrap();
        assert_eq!(verdict.beats.len(), 18);
        assert_eq!(verdict.blips, 1);
        assert_eq!(verdict.widest_gap_ms, 250);
    }

    #[test]
    fn a_cpu_absent_from_two_consecutive_settled_beats_has_stopped() {
        let mut beats: Vec<String> = (1..=3).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
        beats.extend((1..=6).map(|i| beat(2750 + i * 250, 0xdf, 43, 250)));
        let capture = settled_capture(&beats);
        assert_eq!(
            settle(&lines(&capture), &started()),
            Err(Refused::CpuMissing { cpus: vec![5], settled: 9, opened: started().len() + 1 })
        );
    }

    /// A late beat is a period no CPU reached the idle loop in; an empty one is
    /// a period no CPU ran a task in. Both close the window, and what follows
    /// is not read as a CPU.
    #[test]
    fn a_beat_the_machine_did_not_run_through_closes_the_window() {
        for (gap_ms, ran) in [(600, 43), (250, 0)] {
            let mut beats: Vec<String> =
                (1..=4).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
            let stalled = beat(3650, 0xdf, ran, gap_ms);
            beats.push(stalled.clone());
            beats.extend((1..=6).map(|i| beat(3650 + i * 250, 0xdf, 43, 250)));
            let capture = settled_capture(&beats);
            let lines = lines(&capture);
            let at = lines.iter().position(|l| *l == stalled).unwrap();
            assert_eq!(
                settle(&lines, &started()),
                Err(Refused::NotRunning { beat: Beat::parse(at, &stalled).unwrap(), held: 4 })
            );
        }
    }

    /// `LATE`, both sides of it, in milliseconds rather than in the constant: a
    /// 0.500 s gap is two 250 ms periods and the machine ran through it, and a
    /// 0.600 s gap is a period in which no CPU reached the idle loop at all —
    /// five `diag-tick` wakes missed — and closes the window.
    #[test]
    fn two_periods_of_gap_is_run_through_and_more_than_two_is_not() {
        let settled = |gap_ms| {
            let mut beats: Vec<String> =
                (1..=4).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
            beats.push(beat(3250, 0xff, 43, gap_ms));
            settle(&lines(&settled_capture(&beats)), &started()).map(|s| s.beats.len())
        };
        assert_eq!(settled(500), Ok(5));
        assert!(matches!(settled(600), Err(Refused::NotRunning { held: 4, .. })));
    }

    #[test]
    fn a_cpu_that_stopped_before_the_stall_is_still_the_finding() {
        let mut beats: Vec<String> = (1..=4).map(|i| beat(2000 + i * 250, 0xdf, 43, 250)).collect();
        beats.push(beat(3400, 0xdf, 43, 600));
        let capture = settled_capture(&beats);
        assert_eq!(
            settle(&lines(&capture), &started()),
            Err(Refused::CpuMissing { cpus: vec![5], settled: 4, opened: started().len() + 1 })
        );
    }

    /// A verdict about a CPU is read from `MIN_SETTLED` settled beats or from
    /// none: a stall two beats in, and a capture that ends three beats in, each
    /// say why the window is short and neither names a CPU.
    #[test]
    fn a_cpu_missing_from_a_window_shorter_than_the_minimum_is_not_named() {
        let mut beats: Vec<String> = (1..=2).map(|i| beat(2000 + i * 250, 0xdf, 43, 250)).collect();
        beats.push(beat(3400, 0xdf, 43, 600));
        let capture = settled_capture(&beats);
        let stalled = lines(&capture);
        let at = stalled.iter().position(|l| l.contains("t=3.400s")).unwrap();
        assert_eq!(
            settle(&stalled, &started()),
            Err(Refused::NotRunning { beat: Beat::parse(at, stalled[at]).unwrap(), held: 2 })
        );

        let beats: Vec<String> = (1..=3).map(|i| beat(2000 + i * 250, 0xdf, 43, 250)).collect();
        assert_eq!(
            settle(&lines(&settled_capture(&beats)), &started()),
            Err(Refused::Unsettled { settled: 3, beats: 4 })
        );
    }

    #[test]
    fn a_program_that_never_finishes_starting_refuses_the_whole_capture() {
        let without: Vec<&str> = lines(SHARD_8_SUITE)
            .into_iter()
            .filter(|l| !l.contains("exit: sshd"))
            .collect();
        assert_eq!(
            settle(&without, &started()),
            Err(Refused::BootUnfinished("exit: sshd pid=".to_string()))
        );
        assert_eq!(window_beats(&without, &started()), 0);
    }

    /// The beat after the last done line straddles it, so the window opens on
    /// the one after that; a done line after every beat opens nothing.
    #[test]
    fn the_window_opens_on_the_first_whole_period_after_the_last_done_line() {
        let started = started();
        let beats: Vec<String> = (1..=6).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
        let mut capture: String = started[..5].iter().map(|s| format!("{s}\n")).collect();
        capture.push_str(&beats[0]);
        capture.push_str("\n===READY===\n");
        for b in &beats[1..] {
            capture.push_str(b);
            capture.push('\n');
        }
        let verdict = settle(&lines(&capture), &started).unwrap();
        assert_eq!(verdict.beats[0].t_ms, 2750);
        assert_eq!(verdict.beats.len(), 4);
        assert_eq!(window_beats(&lines(&capture), &started), 4);

        let mut capture: String = started[..5].iter().map(|s| format!("{s}\n")).collect();
        for b in &beats {
            capture.push_str(b);
            capture.push('\n');
        }
        capture.push_str("===READY===\n");
        assert_eq!(
            settle(&lines(&capture), &started),
            Err(Refused::Unsettled { settled: 0, beats: 6 })
        );
    }

    #[test]
    fn fewer_settled_beats_than_the_minimum_is_not_a_verdict() {
        let beats: Vec<String> = (1..=3).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
        let capture = settled_capture(&beats);
        assert_eq!(window_beats(&lines(&capture), &started()), 3);
        assert_eq!(
            settle(&lines(&capture), &started()),
            Err(Refused::Unsettled { settled: 3, beats: 4 })
        );
    }

    /// What `CAPTURE_BEATS` buys over the floor: cut at `MIN_SETTLED` the
    /// capture ends on cpu5's first absence, which is a blip and names nobody;
    /// taken to `CAPTURE_BEATS` the second absence lands inside it.
    #[test]
    fn the_capture_carries_beats_past_the_floor_for_the_convicting_one() {
        let mut beats: Vec<String> =
            (1..MIN_SETTLED as u64).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
        beats.push(beat(2000 + MIN_SETTLED as u64 * 250, 0xdf, 43, 250));
        let cut = settled_capture(&beats);
        assert_eq!(window_beats(&lines(&cut), &started()), MIN_SETTLED);
        assert_eq!(settle(&lines(&cut), &started()).unwrap().blips, 1);

        beats.extend(
            (MIN_SETTLED as u64 + 1..=CAPTURE_BEATS as u64)
                .map(|i| beat(2000 + i * 250, 0xdf, 43, 250)),
        );
        let whole = settled_capture(&beats);
        assert_eq!(window_beats(&lines(&whole), &started()), CAPTURE_BEATS);
        assert_eq!(
            settle(&lines(&whole), &started()),
            Err(Refused::CpuMissing {
                cpus: vec![5],
                settled: CAPTURE_BEATS,
                opened: started().len() + 1
            })
        );
    }

    #[test]
    fn a_beat_with_an_unreadable_field_is_refused_by_line() {
        let torn = "[kernel 2.5 cpu2] heartbeat: t=2.500s alive=8/8 mask=0xff ran=43 gap=0.2";
        let mut capture = settled_capture(&[]);
        capture.push_str(torn);
        capture.push('\n');
        assert_eq!(
            settle(&lines(&capture), &started()),
            Err(Refused::Unreadable(torn.to_string()))
        );
        assert_eq!(millis("0.251s"), Some(251));
        assert_eq!(millis("12.000s"), Some(12_000));
        assert_eq!(millis("0.25s"), None);
        assert_eq!(millis("0.251"), None);
    }

    /// The `mask=` is 64 bits, so an `alive=` denominator at 64 or above is no
    /// reading of it and neither is zero — and the refusal is the contract's,
    /// not a shift overflow's.
    #[test]
    fn a_cpu_count_the_mask_cannot_carry_is_unreadable() {
        for alive in ["8/64", "8/100", "8/4294967296", "0/0"] {
            let wide = format!(
                "[kernel 2.5 cpu2] heartbeat: t=2.500s alive={alive} mask=0xff ran=43 gap=0.250s"
            );
            let mut capture = settled_capture(&[]);
            capture.push_str(&wide);
            capture.push('\n');
            assert_eq!(
                settle(&lines(&capture), &started()),
                Err(Refused::Unreadable(wide.clone())),
                "{alive}"
            );
        }
    }

    /// One capture is one machine: a beat whose `alive=` denominator is not the
    /// first's describes a different one, and a mask read against the wrong
    /// width is a CPU invented or a CPU dropped.
    #[test]
    fn a_capture_whose_cpu_count_changes_is_unreadable() {
        let odd = "[kernel 3.0 cpu2] heartbeat: t=3.000s alive=7/7 mask=0x7f ran=43 gap=0.250s";
        let mut beats: Vec<String> = (1..=2).map(|i| beat(2000 + i * 250, 0xff, 43, 250)).collect();
        beats.push(odd.to_string());
        beats.extend((1..=4).map(|i| beat(3000 + i * 250, 0xff, 43, 250)));
        let capture = settled_capture(&beats);
        assert_eq!(settle(&lines(&capture), &started()), Err(Refused::Unreadable(odd.to_string())));
    }

    /// The one table, held against the config it transcribes: a `[boot] start`
    /// program it does not know is refused by name, here and not at the guest.
    #[test]
    fn a_program_the_done_table_does_not_know_is_refused() {
        let known: Vec<String> = DONE.iter().map(|(program, _)| (*program).to_string()).collect();
        assert_eq!(done_lines(&known).unwrap(), started());

        let mut added = known.clone();
        added.push("sniffer".to_string());
        assert!(done_lines(&added).unwrap_err().contains("sniffer"));

        let mut dropped = known;
        dropped.pop();
        assert!(done_lines(&dropped).is_err());
    }
}
