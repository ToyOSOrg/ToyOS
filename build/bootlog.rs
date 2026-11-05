//! What a boot's own log says about that boot, and nothing else: the records
//! that make a boot a pass, and the grammar for reading them.
//!
//! **A metal boot is judged by what survives it**: the log partition — the
//! loader's file, `logkeeper`'s files and its stop mark — and the machine
//! coming back within its bench's recorded time. Nothing a boot leaves in RAM
//! survives where the firmware honours the memory-overwrite request, and what
//! the kernel says after `logkeeper`'s last flush reaches only its console and
//! panel. **What neither half sees**: a hang between the stop mark and the
//! reset — the drivers' stop, the kernel's quiesce, the reset itself — that
//! the boot deadline then ends leaves the same volume as a clean stop, and is
//! told apart only by coming back late by about that deadline.
//!
//! Neither the T14 driver's nor the QEMU harness's — `tests/metal` reads a log
//! off a flashed stick and `tests/common/power.rs` reads one off a guest's log
//! partition, and they must not be able to reach different answers about one
//! log. Text in, a verdict out; its one gate reads the loader's source.

#![forbid(unsafe_code)]

use std::fmt;

/// The word the kernel writes as it hands the machine back to the firmware,
/// in `kernel/src/syscall/machine.rs`'s `quiesce`. On the console and never in
/// `/log`: nothing is left running to write it there.
pub const REBOOTING: &str = "Rebooting.";

/// What the supervisor says as it asks `logkeeper` to make the log whole, before
/// it stops the machine: the last line a passing boot's log is owed, because
/// nothing after it waits for the file.
pub const STOPPING: &str = toyos_logstream::STOPPING;

/// What `tests/guest/test-runner` says when its job list runs past
/// `toyos_tco::JOB_BOUND_MS`, with the job it was inside as the next word,
/// before it kills that job and reboots: a boot that says it is no pass.
pub const JOB_DEADLINE_SAID: &str =
    "test-runner: the job list ran past its bound, and the job it was inside is";

/// What the kernel logs as it arms its boot deadline, in `kernel/src/deadline.rs`.
pub const DEADLINE_ARMED: &str = "boot deadline: ";

/// The bootloader's own file at the root of the log partition.
pub const LOADER_LOG: &str = "loader.log";

/// That file's first line and its last.
pub const LOADER_FIRST_LINE: &str = "ToyOS Bootloader 1.0";
pub const LOADER_LAST_LINE: &str = "Loader log: the kernel handoff begins, so this file ends here";

/// The kernel's record for a process that ended, in `kernel/src/process.rs`.
///
/// **The one channel a guest binary's verdict crosses on a machine with no
/// serial port that is the kernel's own**: a program's output reaches the
/// stick as its lines under its name, and this is the kernel's record of how it
/// ended, which no program writes ([`is_program_line`]).
pub const EXIT: &str = "exit: ";

/// One rendered record's message: what follows its head. `None` for a line
/// that is not a kernel record's first.
pub fn message(line: &str) -> Option<&str> {
    toyos_logstream::parse(line).filter(|p| p.source == toyos_logstream::Source::Kernel).map(|p| p.text)
}

/// Whether a line of the log is a program's (`toyos_logstream::ProgramLine`):
/// `logkeeper` writes that head, and it names no kernel.
pub fn is_program_line(line: &str) -> bool {
    toyos_logstream::is_program_line(line)
}

/// A log without its programs' lines: what a judge of the kernel's own reads.
pub fn kernel_records(log: &str) -> String {
    log.split_inclusive('\n').filter(|line| !is_program_line(line)).collect()
}

/// The AP bring-up record, in `kernel/src/arch/x86_64/smp.rs`. A reader asks for the
/// trailing ` online` as a separate word: the same head carries the failure.
pub const AP_BRINGUP: &str = "SMP: AP cpu";

/// `kernel/src/process.rs`'s `THREAD_NAME_LEN`, one byte of which is the
/// terminator `make_name` leaves.
const NAME_LEN: usize = 28;

/// A process's name as the kernel records it: the path's last component,
/// truncated to what [`NAME_LEN`] holds.
///
/// **A predicate looking for the whole name finds nothing on a good boot**:
/// `test_rs_lent_device_memory_refused` is `test_rs_lent_device_memory_` on the wire.
pub fn recorded_name(binary: &str) -> String {
    let base = binary.rsplit('/').next().unwrap_or(binary);
    base[..base.len().min(NAME_LEN - 1)].to_string()
}

/// Whether `name` on the log volume is one of `logkeeper`'s files, which is
/// `logkeeper`'s own allow-list and not a suffix: the loader's file ends in `.log`
/// too, and a `toybox` run can leave anything there.
pub fn is_logkeeper_file(name: &str) -> bool {
    toyos_wallclock::classify(name).is_some()
}

/// The names on a mounted log volume, split into the loader's file and
/// `logkeeper`'s in the order theirs sort.
///
/// The loader's is matched without case, because a FAT driver that does not
/// read the lowercase flags in a directory entry yields `LOADER.LOG`; `logkeeper`'s
/// are matched as its own writer spells them, which no such driver preserves
/// either — a volume read through one has no `logkeeper` file this can name, and
/// says so by finding none.
pub fn split_listing(listing: &str) -> (Option<&str>, Vec<&str>) {
    let mut loader = None;
    let mut logkeeper = Vec::new();
    for name in listing.lines().map(str::trim).filter(|name| !name.is_empty()) {
        if name.eq_ignore_ascii_case(LOADER_LOG) {
            loader = Some(name);
        } else if is_logkeeper_file(name) {
            logkeeper.push(name);
        }
    }
    logkeeper.sort_unstable();
    (loader, logkeeper)
}

/// Whether a mounted log volume's listing carries `logkeeper`'s stop mark
/// ([`toyos_logstream::STOPPED`]): the boot made its log durable at its stop.
/// Matched without case, as [`LOADER_LOG`] is.
pub fn stop_marked(listing: &str) -> Result<(), Unfit> {
    match listing.lines().map(str::trim).any(|name| name.eq_ignore_ascii_case(toyos_logstream::STOPPED)) {
        true => Ok(()),
        false => Err(Unfit::NoStopMark),
    }
}

/// The parts of its own log the boot `log` ends in no longer has, first and
/// last, or `None` for a log that is whole.
///
/// **Read off the sequence of parts the file itself holds**, which no flush
/// and no death can take without taking the part: `logkeeper` rotates once a
/// round, after that round's write, and the line saying so
/// ([`toyos_logstream::LOG_CONTINUES`]) is written by the next round into the
/// part it opened. So the parts a boot's log names, from the one its opening
/// line names, step by one, and a step that is longer is the parts that are
/// gone. The newest rotation's line may not have reached the file; the part it
/// opened is no hole. A program's line can carry the same words, so only
/// `logkeeper`'s are read, and only for this boot's stem.
pub fn lost_parts(log: &str) -> Option<(u32, u32)> {
    use toyos_logstream::{LOGKEEPER, LOG_CONTINUES, LOG_OPENED};
    use toyos_wallclock::Part;
    fn named(path: &str) -> Option<Part<'_>> {
        Part::parse(path.rsplit('/').next()?).map(|(_, part)| part)
    }
    let mut newest: Option<Part> = None;
    let mut lost: Option<(u32, u32)> = None;
    for said in log.lines().filter_map(toyos_logstream::program_line).filter(|said| said.tag == LOGKEEPER) {
        if let Some(opened) = said.text.strip_prefix(LOG_OPENED) {
            newest = opened.split_whitespace().next().and_then(named);
            lost = None;
            continue;
        }
        let Some((_, path)) = said.text.split_once(LOG_CONTINUES) else { continue };
        let (Some(next), Some(was)) = (named(path), newest) else { continue };
        if next.stem != was.stem {
            continue;
        }
        if next.part > was.part + 1 {
            lost = Some((lost.map_or(was.part + 1, |(first, _)| first), next.part - 1));
        }
        newest = Some(next);
    }
    lost
}

/// The kernel's boot-phase record for the end of boot, in
/// `kernel/src/log/mod.rs`'s `boot_phase!`.
pub const COMPLETE: &str = "Boot: complete (";

/// Why a log is not a passing boot's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    NoBootRecord,
    /// The log carries no word from the supervisor that the machine stops: the last
    /// line it carries instead.
    Unfinished(String),
    /// The volume carries no stop mark: `logkeeper` never made the log
    /// durable at a stop.
    NoStopMark,
    /// The test runner's word that its job list ran past its bound: the
    /// line, naming the job it killed.
    JobsPastBound(String),
}

impl fmt::Display for Unfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBootRecord => write!(f, "the log carries no `{COMPLETE}Nms)` record"),
            Self::Unfinished(saw) => write!(
                f,
                "the log's last line is {saw:?} and the supervisor never said {STOPPING:?}: either the \
                 boot never asked to hand the machine back to the firmware, or logkeeper never made \
                 the log whole before it did"
            ),
            Self::NoStopMark => write!(
                f,
                "the log partition carries no `{}`: logkeeper never made the log durable at a stop",
                toyos_logstream::STOPPED
            ),
            Self::JobsPastBound(said) => write!(
                f,
                "the test runner killed a job and rebooted at its bound: {said:?}"
            ),
        }
    }
}

/// The supervisor's line saying the machine stops, the last one `log` carries.
pub fn stopping_line(log: &str) -> Option<&str> {
    log.lines().rfind(|line| {
        toyos_logstream::program_line(line)
            .is_some_and(|said| said.tag == "supervisor" && said.text.starts_with(STOPPING))
    })
}

/// Whether the stop `log` ends on is a power-off: the supervisor's line names
/// the stop it was asked for, after [`STOPPING`].
pub fn asked_to_power_off(log: &str) -> bool {
    stopping_line(log)
        .and_then(toyos_logstream::program_line)
        .is_some_and(|said| said.text.strip_prefix(STOPPING) == Some(POWER_OFF_ASKED))
}

/// What the supervisor's stop line says after [`STOPPING`] when the stop is a
/// power-off: its `Stop::Shutdown`, debug-printed in parentheses.
const POWER_OFF_ASKED: &str = " (Shutdown)";

/// The loader's word for its clock, in `loader/src/main.rs`: the head of
/// the line naming the rate the CPU states, and the line where it states none.
pub const LOADER_CLOCK_STATED: &str =
    "Loader clock: each line opens with the seconds since the counter's zero, at the counter's stated ";
pub const LOADER_CLOCK_NONE: &str =
    "Loader clock: this CPU states no counter rate, so no line carries the time it was said";

/// The kernel's record of starting `logkeeper`, which every boot does, and the
/// supervisor's line once that spawn has returned to it.
pub const LOGKEEPER_SPAWN: &str = "spawn: /system/bin/logkeeper ";
pub const LOGKEEPER_STARTED: &str = "supervisor: started logkeeper";

/// Whether the loader's lines, the kernel's records and a program's lines
/// count from one zero: no timed kernel record is earlier than the loader's
/// last timed line before the handoff, and the supervisor's
/// [`LOGKEEPER_STARTED`] is no earlier than the kernel's [`LOGKEEPER_SPAWN`],
/// which the same spawn call writes milliseconds before. A clock that kept
/// another zero puts one of them before what caused it.
///
/// `loader` holds the pass that handed the machine to the kernel; `log` holds
/// the kernel's records and the programs' lines, and may hold the loader's
/// too, as a console does. Nothing to compare is a refusal wherever the boot
/// owes it: a loader that states a rate owes timed lines, and a boot that
/// reached [`COMPLETE`] owes the spawn record and the supervisor's line.
pub fn one_clock(loader: &str, log: &str) -> Result<(), String> {
    use toyos_logstream::{parse, Source};
    let handed = loader
        .find(LOADER_LAST_LINE)
        .map(|at| &loader[..at + LOADER_LAST_LINE.len()])
        .ok_or_else(|| format!("the loader never said {LOADER_LAST_LINE:?}"))?;
    let stated = handed.contains(LOADER_CLOCK_STATED);
    if !stated && !handed.contains(LOADER_CLOCK_NONE) {
        return Err("the loader said nothing of its clock before the handoff".to_string());
    }
    if stated {
        let loader = handed
            .lines()
            .filter_map(parse)
            .filter(|p| p.source == Source::Loader)
            .filter_map(|p| p.ms)
            .next_back()
            .ok_or("the loader states its counter's rate and none of its lines carries a time")?;
        let kernel = log
            .lines()
            .filter_map(record_millis)
            .min()
            .ok_or("the loader states its counter's rate and no kernel record carries a time")?;
        if kernel < loader {
            return Err(format!(
                "the loader's last line before the handoff reads {loader} ms and the kernel's \
                 earliest timed record {kernel} ms"
            ));
        }
    }
    let spawned = log.lines().find(|l| message(l).is_some_and(|m| m.starts_with(LOGKEEPER_SPAWN))).and_then(record_millis);
    let said = log
        .lines()
        .filter_map(parse)
        .find(|p| p.source == Source::Program("supervisor") && p.text == LOGKEEPER_STARTED)
        .and_then(|p| p.ms);
    let complete = log.lines().any(|l| message(l).is_some_and(|m| m.starts_with(COMPLETE)));
    match (spawned, said) {
        (Some(spawned), Some(said)) if said < spawned => Err(format!(
            "the supervisor's {LOGKEEPER_STARTED:?} reads {said} ms and the kernel's record of that spawn {spawned} ms"
        )),
        (Some(_), Some(_)) => Ok(()),
        (spawned, said) if complete => Err(format!(
            "the boot reached {COMPLETE:?} with {} and {}",
            if spawned.is_some() { "a timed spawn record of logkeeper" } else { "no timed spawn record of logkeeper" },
            if said.is_some() { "a timed supervisor line saying so" } else { "no timed supervisor line saying so" },
        )),
        _ => Ok(()),
    }
}

/// The milliseconds since the counter's zero one record line carries, whether
/// `logkeeper` put a wall clock before them or the panel put nothing.
pub fn record_millis(line: &str) -> Option<u64> {
    toyos_logstream::record_ms(line)
}

/// Whether `source` declares a constant whose value is exactly `rhs`, wrapped
/// or not.
///
/// **The only way two crates nothing links are held to one spelling.** The line
/// must end `= <rhs>;`, so a name in a message or inside a longer literal is not
/// a declaration, and rustfmt's wrap of a value too wide for its line still is.
pub fn declares(source: &str, rhs: &str) -> bool {
    let tail = format!("= {rhs};");
    let mut joined = String::new();
    for line in source.lines() {
        let line = line.trim_end();
        if joined.ends_with('=') {
            joined.push(' ');
            joined.push_str(line.trim_start());
            continue;
        }
        joined.push('\n');
        joined.push_str(line);
    }
    joined.lines().any(|line| line.trim_end().ends_with(&tail))
}

/// When the last record in `log` was written, in milliseconds since the
/// counter's zero.
pub fn last_record_millis(log: &str) -> Option<u64> {
    log.lines().rev().find_map(record_millis)
}

/// The boot's own duration, out of `Boot: complete (123ms)`.
pub fn boot_millis(log: &str) -> Option<u64> {
    let tail = log.lines().filter(|l| !is_program_line(l)).find_map(|line| line.split(COMPLETE).nth(1))?;
    tail.split("ms)").next()?.parse().ok()
}

/// A boot's duration if its log is a passing boot's, which takes both lines:
/// the kernel's boot record, and the supervisor's word that the machine stops — said
/// before `logkeeper` made the log whole, so a log that carries it is whole to it.
/// That the stop then made the log durable is [`stop_marked`]'s to say, and that
/// the reset came is the console's, or whoever watched the machine come back.
pub fn verdict(log: &str) -> Result<u64, Unfit> {
    let boot_ms = boot_millis(log).ok_or(Unfit::NoBootRecord)?;
    let past_bound = |line: &&str| {
        toyos_logstream::program_line(line).is_some_and(|said| said.tag == "test-runner" && said.text.starts_with(JOB_DEADLINE_SAID))
    };
    if let Some(line) = log.lines().find(past_bound) {
        return Err(Unfit::JobsPastBound(line.trim().to_string()));
    }
    if stopping_line(log).is_none() {
        let last = log.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
        return Err(Unfit::Unfinished(last.trim().to_string()));
    }
    Ok(boot_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half-told boot: the kernel got all the way up and the log stops
    /// there, so the machine either never asked for the reset or `logkeeper` never
    /// made the log whole before it.
    #[test]
    fn a_boot_record_without_the_supervisors_stop_is_not_a_pass() {
        let booted = "[ 1.151 cpu0 kernel] Boot: complete (1151ms)\n";
        let stopping = format!("[ 1.203 supervisor] {STOPPING} (Reboot)\n");
        let ended = format!("{booted}{stopping}");
        assert_eq!(verdict(&ended), Ok(1151));
        // What `logkeeper` wrote between the flush and the stop is no refusal.
        assert_eq!(verdict(&format!("{ended}[ 1.210 cpu0 kernel] exit: reboot pid=6\n")), Ok(1151));

        assert_eq!(
            verdict(booted),
            Err(Unfit::Unfinished("[ 1.151 cpu0 kernel] Boot: complete (1151ms)".to_string()))
        );
        // The words, from anyone but the supervisor, and from the supervisor as anything but its
        // line, are not the supervisor's stop.
        let forged = format!("{booted}[ 1.203 test-runner] {STOPPING}\n");
        assert!(matches!(verdict(&forged), Err(Unfit::Unfinished(_))));
        let quoted = format!("{booted}[ 1.203 supervisor] supervisor: said {STOPPING}\n");
        assert!(matches!(verdict(&quoted), Err(Unfit::Unfinished(_))));
        assert_eq!(verdict(&stopping), Err(Unfit::NoBootRecord));
        assert_eq!(verdict(""), Err(Unfit::NoBootRecord));
    }

    /// A boot the test runner ended at its job bound stopped as a passing
    /// one does, and is none.
    #[test]
    fn a_boot_whose_job_list_ran_past_its_bound_is_not_a_pass() {
        let past = format!("[2026-11-04 17:07:50 535.506 test-runner tid=1] {JOB_DEADLINE_SAID} osbench_persist (525000 ms)\n");
        let log = format!(
            "[ 1.151 cpu0 kernel] Boot: complete (1151ms)\n{past}[ 535.507 supervisor] {STOPPING} (Reboot)\n"
        );
        assert_eq!(verdict(&log), Err(Unfit::JobsPastBound(past.trim().to_string())));
        let quoted = log.replace("test-runner tid=1] ", "osbench tid=4] said ");
        assert_eq!(verdict(&quoted), Ok(1151));
    }

    /// Only the supervisor's own stop line, naming a shutdown, is a power-off.
    #[test]
    fn a_power_off_is_the_supervisors_stop_naming_a_shutdown() {
        let booted = "[ 1.151 cpu0 kernel] Boot: complete (1151ms)\n";
        let stop = |how: &str| format!("{booted}[16.705 supervisor] {STOPPING} ({how})\n");
        assert!(asked_to_power_off(&stop("Shutdown")));
        assert!(!asked_to_power_off(&stop("Reboot")));
        assert!(!asked_to_power_off(booted));
        let forged = format!("{booted}[16.705 test-runner] {STOPPING} (Shutdown)\n");
        assert!(!asked_to_power_off(&forged));
    }

    #[test]
    fn only_a_declaration_of_the_whole_value_counts() {
        assert!(declares("const A: &str = \"x\";", "\"x\""));
        assert!(declares("    const A: &CStr16 = cstr16!(\"x\");   ", "cstr16!(\"x\")"));
        // A longer literal that carries the value, and a mention in a message.
        assert!(!declares("const A: &str = \"xy\";", "\"x\""));
        assert!(!declares("    say(\"x\");", "\"x\""));
        // The value under another spelling, and concatenated.
        assert!(!declares("const A: &CStr16 = cstr16!(\"x\");", "\"x\""));
        assert!(!declares("const A: &str = \"x\" \"y\";", "\"xy\""));
        // Wrapped by rustfmt, which is how the widest of them is written.
        assert!(declares("const A: &str =\n    \"x\";", "\"x\""));
    }

    /// Nothing links the two crates: the loader is `no_std` and this is the
    /// build system, so every name above is held to the loader's own
    /// declarations by reading its source.
    #[test]
    fn the_loader_writes_the_lines_the_host_reads() {
        let wanted = [
            ("loader/src/loaderlog.rs", format!("cstr16!(\"{LOADER_LOG}\")")),
            ("loader/src/loaderlog.rs", format!("\"{LOADER_FIRST_LINE}\"")),
            ("loader/src/loaderlog.rs", format!("\"{LOADER_LAST_LINE}\"")),
        ];
        for (file, rhs) in wanted {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
            let source = std::fs::read_to_string(&path).expect("a loader module");
            assert!(
                declares(&source, &rhs),
                "{} declares no constant equal to {rhs}",
                path.display()
            );
        }
        // Formats, not constants: the loader fills the rate's hole with the
        // counter's.
        let main = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("loader/src/main.rs"),
        )
        .expect("the loader's main");
        for said in [format!("{LOADER_CLOCK_STATED}{{hz}} Hz\""), format!("\"{LOADER_CLOCK_NONE}\"")] {
            assert!(main.contains(&said), "loader/src/main.rs prints no {said:?}");
        }
    }

    #[test]
    fn the_loaders_file_is_told_from_logkeepers_however_a_driver_spelled_it() {
        let listing = "2026-09-06-084003.log\nloader.log\nunknown-00.log\nnotes.txt\n";
        assert_eq!(
            split_listing(listing),
            (Some("loader.log"), vec!["2026-09-06-084003.log", "unknown-00.log"])
        );
        // A FAT driver that drops the lowercase flags yields 8.3 in upper case.
        assert_eq!(split_listing("LOADER.LOG\n").0, Some("LOADER.LOG"));
        // And it is never one of logkeeper's, under either spelling.
        assert!(split_listing("LOADER.LOG\nloader.log\n").1.is_empty());
        // Blank rows and stray whitespace are a listing's, not a name's.
        assert_eq!(split_listing("\n  loader.log  \n\n").0, Some("loader.log"));
        assert_eq!(split_listing(""), (None, Vec::new()));
        // Somebody else's file, which nothing here may name or delete.
        assert_eq!(split_listing("boot.log\n"), (None, Vec::new()));
    }

    /// The stop mark is `logkeeper`'s one name, under either spelling a FAT
    /// driver yields, and no other file is it.
    #[test]
    fn a_stopped_boot_is_told_by_its_mark() {
        assert_eq!(stop_marked("loader.log\nstopped\n"), Ok(()));
        assert_eq!(stop_marked("LOADER.LOG\nSTOPPED\n"), Ok(()));
        assert_eq!(stop_marked("loader.log\nstopped.log\nbooted\n"), Err(Unfit::NoStopMark));
        assert_eq!(stop_marked(""), Err(Unfit::NoStopMark));
    }

    /// The two kernel spellings a metal readback is judged on, and the length
    /// it truncates a name to — held to the kernel's own source, because
    /// nothing links this crate to it either.
    #[test]
    fn the_kernel_writes_the_records_the_host_reads() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (file, needle) in [
            ("kernel/src/process.rs", format!("log!(\"{EXIT}{{name}} pid=")),
            ("kernel/src/arch/x86_64/smp.rs", format!("log!(\"{AP_BRINGUP}")),
            ("kernel/src/process.rs", format!("THREAD_NAME_LEN: usize = {NAME_LEN}")),
            ("kernel/src/deadline.rs", format!("\"{DEADLINE_ARMED}{{ms}} ms")),
        ] {
            let at = root.join(file);
            let source = std::fs::read_to_string(&at).expect("a kernel module");
            assert!(source.contains(&needle), "{} does not write {needle:?}", at.display());
        }
    }

    use toyos_logstream::{LOG_CONTINUES, LOG_OPENED};

    const STEM: &str = "2026-10-08-140646";

    fn opened(stem: &str) -> String {
        format!("[2026-10-08 14:06:46 12.841 logkeeper] {LOG_OPENED}/log/{stem}.log (2026-10-08 14:06:46 UTC)\n")
    }

    /// `logkeeper`'s line for the rotation that opened part `to` of `stem`.
    fn continued(tag: &str, stem: &str, to: u32) -> String {
        let part = |part| toyos_wallclock::Part { stem, part };
        format!(
            "[2026-10-08 14:07:21 47.242 {tag}] logkeeper: /log/{} reached 1067282{LOG_CONTINUES}/log/{}\n",
            part(to - 1),
            part(to)
        )
    }

    fn parts(stem: &str, to: std::ops::RangeInclusive<u32>) -> String {
        to.map(|to| continued("logkeeper", stem, to)).collect()
    }

    /// **The hole no deletion line names**: a boot's file holds the rotation
    /// that opened each part it has, so parts 1 and 3 with nothing said of a
    /// deletion is part 2 gone — whether the line saying so was held back by
    /// the stop's flush or the boot died before the round that would write it.
    #[test]
    fn a_part_whose_rotation_the_log_does_not_hold_is_a_hole() {
        assert_eq!(lost_parts(&format!("{}{}", opened(STEM), continued("logkeeper", STEM, 3))), Some((2, 2)));
        // Died between the removal of part 2 and the next sync: parts 1 and 3
        // to 16 came back, and part 17 was never opened.
        assert_eq!(lost_parts(&format!("{}{}", opened(STEM), parts(STEM, 3..=16))), Some((2, 2)));
        // The stop's flush held back the newest rotation's line: part 17 is on
        // the volume and nothing in the file says so.
        assert_eq!(lost_parts(&format!("{}{}", opened(STEM), parts(STEM, 4..=16))), Some((2, 3)));
    }

    /// What came back of the T14 boot that wrote forty-one parts, a boot that
    /// rotated and lost nothing, and the lines that are not this boot's.
    #[test]
    fn a_boot_that_lost_parts_of_its_own_log_is_told_from_one_that_is_whole() {
        let own = opened(STEM);
        assert_eq!(lost_parts(&format!("{own}{}", parts(STEM, 27..=41))), Some((2, 26)));
        // Two holes are reported from the first lost part to the last.
        assert_eq!(lost_parts(&format!("{own}{}{}", parts(STEM, 3..=4), parts(STEM, 9..=10))), Some((2, 8)));

        assert_eq!(lost_parts(&format!("{own}{}", parts(STEM, 2..=8))), None);
        assert_eq!(lost_parts(&own), None);
        assert_eq!(lost_parts(""), None);
        // The same words from another program, and about another boot's file.
        let quoted = format!(
            "{own}{}{}",
            continued("test-runner pid=31", STEM, 12),
            continued("logkeeper", "2026-10-07-091500", 5)
        );
        assert_eq!(lost_parts(&quoted), None);
        // The boot the log ends in: an earlier boot's hole on the same volume
        // is that boot's.
        let next = "2026-10-08-141929";
        assert_eq!(lost_parts(&format!("{own}{}{}", parts(STEM, 27..=41), opened(next))), None);
        assert_eq!(
            lost_parts(&format!("{own}{}{}{}", parts(STEM, 2..=3), opened(next), continued("logkeeper", next, 4))),
            Some((2, 3))
        );
    }

    /// A program writing a kernel verdict's words, or another program's head,
    /// is left out of the kernel's records; a kernel record's continuation
    /// line is the kernel's.
    #[test]
    fn a_programs_line_is_never_the_kernels() {
        let log = "[2026-09-08 16:08:23  2.100 cpu0 kernel] exit: test_rs_job pid=4 code=3 cpu=1ms\n\
                   [2026-09-08 16:08:23  2.150 cpu0 kernel] PANIC: a report\n  its second line\n\
                   [2026-09-08 16:08:23  2.200 test-runner] [2026-09-08 16:08:23  2.200 cpu0 kernel] exit: test_rs_job pid=4 code=0 cpu=0ms\n\
                   [2026-09-08 16:08:23  2.300 test-runner] [ 2.300 netstack] netstack: MAC 00:00:00:00:00:00\n\
                   [2026-09-08 16:08:23  2.400 netstack] netstack: MAC 52:54:00:12:34:56\n";
        let kernel = kernel_records(log);
        assert!(!kernel.contains("code=0"), "{kernel}");
        assert!(kernel.contains("code=3") && kernel.contains("  its second line\n"), "{kernel}");
        assert!(is_program_line(log.lines().nth(3).expect("five lines")));
    }

    /// The truncation, which is what a whole-name predicate would miss.
    #[test]
    fn a_long_binarys_record_is_the_prefix_the_kernel_keeps() {
        assert_eq!(recorded_name("test_rs_readdir_bound"), "test_rs_readdir_bound");
        assert_eq!(recorded_name("test_rs_lent_device_memory_refused"), "test_rs_lent_device_memory_");
        assert_eq!(recorded_name("/system/bin/echo"), "echo");
        assert_eq!(recorded_name("").len(), 0);
    }

    #[test]
    fn the_boot_record_is_the_kernels_own_line() {
        assert_eq!(boot_millis("[ 1.151 cpu0 kernel] Boot: complete (1151ms)\n"), Some(1151));
        assert_eq!(boot_millis("[ 0.084 cpu0 kernel] Boot: storage ready (84ms)\n"), None);
        assert_eq!(boot_millis("Boot: complete (later)\n"), None);
        assert_eq!(boot_millis(""), None);
    }
}

#[cfg(test)]
mod record_time_tests {
    use super::*;

    /// Both writers' shapes: `logkeeper`'s file carries a wall-clock tag before the
    /// elapsed field and the panel carries none, and the same reader answers
    /// for both.
    #[test]
    fn a_records_elapsed_field_is_read_past_whatever_tag_precedes_it() {
        assert_eq!(
            record_millis("[2026-09-07 22:57:46  3.109 cpu1 kernel] exit: a pid=7 code=0 cpu=180ms"),
            Some(3_109)
        );
        assert_eq!(record_millis("[ 3.109 cpu1 kernel] exit: a pid=7 code=0"), Some(3_109));
        assert_eq!(
            record_millis("[2026-09-07 22:58:03 20.071 cpu2 kernel tid=1] exit: b tid=1 code=0"),
            Some(20_071)
        );
        // The `cpu=180ms` field is a record's *content*: a reader keying on the
        // first `cpu` would answer with a duration instead of a timestamp.
        assert_eq!(record_millis("no timestamp here, cpu=1ms"), None);
        assert_eq!(record_millis(""), None);
    }

    #[test]
    fn the_last_record_is_the_last_line_that_carries_a_time() {
        let log = "[ 1.000 cpu0 kernel] first\n[ 2.500 cpu1 kernel] second\nnot a record\n";
        assert_eq!(last_record_millis(log), Some(2_500));
        assert_eq!(last_record_millis("nothing\n"), None);
    }
}
