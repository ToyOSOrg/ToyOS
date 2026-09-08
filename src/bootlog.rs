//! What a boot's own log says about that boot, and nothing else: the two
//! records that make a boot a pass, and the grammar for reading them.
//!
//! Neither the T14 driver's nor the QEMU harness's — `src/metal.rs` reads a log
//! off a flashed stick and `tests/common/power.rs` reads one off a guest's log
//! partition, and they must not be able to reach different answers about one
//! log. Text in, a verdict out; its one gate reads the loader's source.

#![forbid(unsafe_code)]

use std::fmt;

/// The word the kernel writes as it hands the machine back to the firmware,
/// in `kernel/src/arch/syscall/machine.rs`'s `quiesce`.
pub const REBOOTING: &str = "Rebooting.";

/// What `userland/test-runner` says when its job list runs past
/// `toyos_tco::JOB_BOUND_MS`, with the job it was inside as the next word.
/// **Console only**: a userland write reaches the serial backend and never a
/// log record, so no stick carries it.
pub const JOB_DEADLINE_SAID: &str =
    "test-runner: the job list ran past its bound, and the job it was inside is";

/// What the kernel's own boot deadline writes into the black box as it ends the
/// machine, in `kernel/src/deadline.rs`. The loader prints it back under
/// [`PREVIOUS_PANIC`] on the pass after the reset, and that is the only channel
/// it has: a wedged boot's `logd` wrote nothing.
pub const DEADLINE_EXPIRED: &str = "the boot deadline expired";

/// What the `wedge-before-reset` actuator says before it stops every CPU, in
/// `kernel/src/deadline.rs`. The witness that a deadline ended a wedge and not
/// a boot merely slower than its bound, which is what makes that control one.
pub const WEDGE_STAGED: &str = "wedge: staged, and only the boot deadline ends this machine";

/// What the CPU that *stages* that wedge says about the state it arrived in,
/// also in `kernel/src/deadline.rs`.
///
/// **The one line that measures that control's own claim.** It arrives through
/// the shutdown syscall, and `arch::syscall::gate` masks `IF` for the whole of a
/// syscall — so a wedge that inherited its state leaves exactly one CPU per boot
/// taking no interrupt at all, which is not a wedge but a hard lockup. A boot on
/// which no CPU says this is a boot whose wedge never reached the CPU that asked
/// for it.
pub const WEDGE_ARRIVED_DEAF: &str =
    "arrived with interrupts off, through the syscall gate, and takes them again here";

/// What one CPU's own NMI writes into the black box when that CPU has taken no
/// interrupt for its bound, in `kernel/src/hardlockup/mod.rs`.
///
/// The *other* record a machine that stopped can leave, and which of the two it
/// left is most of the verdict: [`DEADLINE_EXPIRED`] is a machine that stopped
/// making progress while some CPU still took interrupts, and this one names a
/// single cpu that stopped taking them and where it was standing when it did —
/// whatever the rest of the machine was doing.
pub const LOCKED_UP: &str = "a cpu locked up with interrupts off";

/// What the `hard-lockup-probe` actuator says before its cpu stops answering,
/// in `kernel/src/hardlockup/probe.rs` — the witness in the sealed record's tail
/// that this machine was ended by the control that was staged on it.
pub const LOCKUP_STAGED: &str = "hard-lockup: staged, and only the lockup detector ends this cpu";

/// What the kernel seals under its own `DONE` record about the log volume, in
/// `kernel/src/log/mod.rs`'s `account_for_durability`. The next loader pass
/// prints it back under [`PREVIOUS_PANIC`].
///
/// **The one reader a boot's own tail has.** `logd` writes the file, so
/// everything said after the volume stopped taking bytes — `logd`'s give-up
/// line, the kernel's own `shutdown: /log did not answer` record, every later
/// `exit:` — is written where no file can carry it, and on a machine with no
/// serial port a console is nothing.
pub const LOG_COMPLETE: &str = "log: /log holds every record this boot committed";
/// The other half of [`LOG_COMPLETE`]: how far the volume got, and how many
/// records committed after that reached it. The newest of them follow under
/// [`LOG_TAIL`].
pub const LOG_SHORT: &str = "log: /log holds this boot to";
/// One record the volume never got, on the black-box page.
pub const LOG_TAIL: &str = "log-tail: ";

/// What the loader says about a boot that reached its own shutdown, in
/// `bootloader/src/blackbox.rs`'s `State::Done` arm.
///
/// **The only boot that owes a log account.** A boot ended by its own deadline
/// or by the lockup detector never reaches `quiesce`, so its log stops early by
/// construction and its record is `Wedged` rather than `Done`; asking such a
/// boot for [`LOG_COMPLETE`] would red the two registrations whose whole
/// subject is that it stopped.
pub const HANDED_BACK: &str = "the last boot read DONE";

/// The bootloader's own file at the root of the log partition.
pub const LOADER_LOG: &str = "loader.log";

/// That file's first line and its last.
pub const LOADER_FIRST_LINE: &str = "ToyOS Bootloader 1.0";
pub const LOADER_LAST_LINE: &str = "Loader log: the kernel handoff begins, so this file ends here";

/// The line the loader prints once it has opened `GraphicsOutput`, which the
/// kernel's own `GOP:` line does not begin with.
pub const LOADER_GOP_LINE: &str = "GOP: mode";

/// The head the loader writes every line about the black-box page under, and
/// the line a harvested report goes under.
pub const BLACKBOX_HEAD: &str = "Black box:";
pub const PREVIOUS_PANIC: &str = "Previous boot's panic:";

/// The loader's last line on a pass that read that page and boots no kernel,
/// which is what tells a chain that ended from one that went round again —
/// [`LOADER_LAST_LINE`] is the other.
pub const CHAIN_ENDS_LINE: &str =
    "Loader log: the last boot is accounted for, so this pass resets the machine";

/// **What a boot that hung looks like from the next one.**
///
/// `bootnext` aims `BootNext` at the loader before every kernel handoff, so a
/// kernel that hangs and an owner who cuts power boot the same kernel again for
/// ever — the power cut is exactly what empties the black box, so the next pass
/// has nothing to report and arms a fresh record. The loader counts attempts on
/// the stick instead, and the second attempt of an image whose first never
/// reported boots no kernel at all: it writes this and hands the machine back to
/// the firmware's own boot order.
pub const HUNG_WITHOUT_A_RECORD: &str =
    "Boot attempts: the previous boot of this image never reported; the machine is handed back";

/// The head the loader writes before the pass that read what the boot above
/// left. **One `toyos-metal` run is one kernel boot and two loader passes**,
/// both in one `loader.log`: the loader points `BootNext` at itself before every
/// handoff, and a pass with a finding appends under this rather than truncating.
pub const SEPARATOR: &str = "--- the pass after the reset, reading what the boot above left";

/// The kernel's record for a process that ended, in `kernel/src/process.rs`.
///
/// **The one channel a guest binary's verdict crosses on a machine with no
/// serial port**: its output reaches `Backend::None`, and this is a log record,
/// so `logd` writes it to the stick.
pub const EXIT: &str = "exit: ";

/// The kernel's record for a process that started, in `kernel/src/process.rs`.
///
/// Read for where it must *not* be: after the boot's own last word, where it
/// says a process still on a run queue started another one under a shutdown.
pub const SPAWN: &str = "spawn: ";

/// The AP bring-up record, in `kernel/src/arch/smp.rs`. A reader asks for the
/// trailing ` online` as a separate word: the same head carries the failure.
pub const AP_BRINGUP: &str = "SMP: AP cpu";

/// `kernel/src/process.rs`'s `THREAD_NAME_LEN`, one byte of which is the
/// terminator `make_name` leaves.
const NAME_LEN: usize = 28;

/// A process's name as the kernel records it: the path's last component,
/// truncated to what [`NAME_LEN`] holds.
///
/// **A predicate looking for the whole name finds nothing on a good boot**:
/// `test_rs_null_sink_client_exits` is `test_rs_null_sink_client_ex` on the wire.
pub fn recorded_name(binary: &str) -> String {
    let base = binary.rsplit('/').next().unwrap_or(binary);
    base[..base.len().min(NAME_LEN - 1)].to_string()
}

/// Whether `name` on the log volume is one of `logd`'s files, which is
/// `logd`'s own allow-list and not a suffix: the loader's file ends in `.log`
/// too, and a `toybox` run can leave anything there.
pub fn is_logd_file(name: &str) -> bool {
    toyos_wallclock::classify(name).is_some()
}

/// The names on a mounted log volume, split into the loader's file and
/// `logd`'s in the order theirs sort.
///
/// The loader's is matched without case, because a FAT driver that does not
/// read the lowercase flags in a directory entry yields `LOADER.LOG`; `logd`'s
/// are matched as its own writer spells them, which no such driver preserves
/// either — a volume read through one has no `logd` file this can name, and
/// says so by finding none.
pub fn split_listing(listing: &str) -> (Option<&str>, Vec<&str>) {
    let mut loader = None;
    let mut logd = Vec::new();
    for name in listing.lines().map(str::trim).filter(|name| !name.is_empty()) {
        if name.eq_ignore_ascii_case(LOADER_LOG) {
            loader = Some(name);
        } else if is_logd_file(name) {
            logd.push(name);
        }
    }
    logd.sort_unstable();
    (loader, logd)
}

/// The kernel's boot-phase record for the end of boot, in
/// `kernel/src/log/mod.rs`'s `boot_phase!`.
const COMPLETE: &str = "Boot: complete (";

/// Why a log is not a passing boot's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    NoBootRecord,
    /// The log does not end at the reset: the last line it carries instead.
    Unfinished(String),
}

impl fmt::Display for Unfit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBootRecord => write!(f, "the log carries no `{COMPLETE}Nms)` record"),
            Self::Unfinished(saw) => write!(
                f,
                "the log's last line is {saw:?} and not {REBOOTING:?}: either the boot never \
                 handed the machine back to the firmware, or the reset outran logd"
            ),
        }
    }
}

/// The milliseconds since boot one record line carries.
///
/// **Found from the CPU it precedes rather than by position**: the field before
/// it is the writer's tag, and the two writers disagree about it on purpose —
/// `logd` puts a wall clock there and the panel puts nothing.
pub fn record_millis(line: &str) -> Option<u64> {
    let (before, _) = line.split_once(" cpu")?;
    // The opening bracket, for the writer that puts no tag before the field.
    let field = before.split_whitespace().next_back()?.trim_start_matches('[');
    let (secs, millis) = field.split_once('.')?;
    let secs: u64 = secs.parse().ok()?;
    let millis: u64 = millis.parse().ok()?;
    secs.checked_mul(1_000)?.checked_add(millis)
}

/// The UTC second one record line carries, as seconds since the epoch.
///
/// **The only field in a log a host clock can be held against.** Everything
/// else a record says is measured from this boot's own start, and a host that
/// wants to know whether something it saw happened *while this boot was up* has
/// nothing to compare that with. `logd` writes the wall clock; the panel writes
/// none, and this answers `None` for those lines rather than reading the
/// milliseconds field as a date.
pub fn record_unix_secs(line: &str) -> Option<u64> {
    const EPOCH: &str = "1970-01-01";
    let mut fields = line.strip_prefix('[')?.split_whitespace();
    let day = crate::day::Day::parse(fields.next()?)?;
    let days = crate::day::Day::parse(EPOCH).expect("the epoch is a date").until(day);
    let (hours, rest) = fields.next()?.split_once(':')?;
    let (minutes, seconds) = rest.split_once(':')?;
    let (hours, minutes, seconds): (i64, i64, i64) =
        (hours.parse().ok()?, minutes.parse().ok()?, seconds.parse().ok()?);
    // A leap second is the one value past the ordinary range that is a time.
    if !(0..24).contains(&hours) || !(0..60).contains(&minutes) || !(0..=60).contains(&seconds) {
        return None;
    }
    u64::try_from(days * 86_400 + hours * 3_600 + minutes * 60 + seconds).ok()
}

/// The span this boot's own records bracket: its first wall clock, and the one
/// on [`REBOOTING`] where the boot got that far.
fn record_unix_span(log: &str) -> Option<(u64, u64)> {
    let first = log.lines().find_map(record_unix_secs)?;
    let last = log.lines().rev().find_map(record_unix_secs)?;
    let ended = log.lines().rfind(|l| l.contains(REBOOTING)).and_then(record_unix_secs);
    Some((first, ended.unwrap_or(last)))
}

/// Whether a second on the *host's* clock fell inside the boot this log is of,
/// at or after the record `after` names.
///
/// **The records are the one place a host clock and a boot's clock meet.**
/// `window` is the host's own clock at the two ends of the span in which the
/// machine was running neither of its operating systems; this boot's records
/// have to fall inside it, which bounds the two clocks' disagreement against
/// the run's own data instead of assuming a bound. How far into that window an
/// observation came separates nothing: the window holds the operating system
/// that left and the one that came back as well as this boot.
pub fn host_second_inside_this_boot(
    log: &str,
    window: (u64, u64),
    after: &str,
    at: u64,
) -> Result<(), String> {
    let (first, ended) = record_unix_span(log).ok_or_else(|| {
        "this log carries no record with a wall clock on it, so there is nothing to hold the \
         host's own clock against"
            .to_string()
    })?;
    let (from, to) = window;
    if first < from || ended > to {
        return Err(format!(
            "this boot's own records run {first}..{ended} and the host watched the machine over \
             {from}..{to}: the two clocks disagree by more than the window is wide, so nothing \
             the host saw can be placed inside this boot"
        ));
    }
    if at < first || at > ended {
        return Err(format!(
            "the host saw it at {at}, outside the {first}..{ended} this boot's own records \
             bracket: it came {} s {} the boot, so it belongs to the operating system on the \
             other side of it",
            if at < first { first - at } else { at - ended },
            if at < first { "before" } else { "after" },
        ));
    }
    let after_at = log
        .lines()
        .find(|l| l.contains(after))
        .and_then(record_unix_secs)
        .ok_or_else(|| format!("this boot has no {after:?} record carrying a wall clock"))?;
    if at < after_at {
        return Err(format!(
            "the host saw it at {at}, {} s before this boot's {after:?} record at {after_at}",
            after_at - at
        ));
    }
    Ok(())
}

/// When the last record in `log` was written, in milliseconds since boot.
pub fn last_record_millis(log: &str) -> Option<u64> {
    log.lines().rev().find_map(record_millis)
}

/// The boot's own duration, out of `Boot: complete (123ms)`.
pub fn boot_millis(log: &str) -> Option<u64> {
    let tail = log.lines().find_map(|line| line.split(COMPLETE).nth(1))?;
    tail.split("ms)").next()?.parse().ok()
}

/// A boot's duration if its log is a passing boot's, which takes both records:
/// a log ending anywhere but the reset is a machine that did not come back on
/// its own, so the word is looked for as the last line and not in the text.
pub fn verdict(log: &str) -> Result<u64, Unfit> {
    let boot_ms = boot_millis(log).ok_or(Unfit::NoBootRecord)?;
    let last = log.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default();
    if !last.contains(REBOOTING) {
        return Err(Unfit::Unfinished(last.trim().to_string()));
    }
    Ok(boot_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half-told boot: the kernel got all the way up and the log stops
    /// there, so the machine either never asked for the reset or the reset
    /// outran `logd`.
    #[test]
    fn a_boot_record_without_the_reset_word_is_not_a_pass() {
        let booted = "[kernel 1.151 cpu0] Boot: complete (1151ms)\n";
        let ended = format!("{booted}[logd 1.203 cpu1] {REBOOTING}\n");
        assert_eq!(verdict(&ended), Ok(1151));
        // Trailing blank lines are not the last line.
        assert_eq!(verdict(&format!("{ended}\n  \n")), Ok(1151));

        assert_eq!(
            verdict(booted),
            Err(Unfit::Unfinished("[kernel 1.151 cpu0] Boot: complete (1151ms)".to_string()))
        );
        let carried_on = format!("{ended}[kernel 1.400 cpu0] hda: codec 0 reset\n");
        assert!(matches!(verdict(&carried_on), Err(Unfit::Unfinished(_))));
        assert_eq!(verdict(&format!("[logd 0.9 cpu1] {REBOOTING}\n")), Err(Unfit::NoBootRecord));
        assert_eq!(verdict(""), Err(Unfit::NoBootRecord));
    }

    /// Whether `source` declares a constant whose value is exactly `rhs`.
    ///
    /// Anchored to the declaration, so a name that appears in a message or in
    /// a longer literal is not one: the line must end `= <rhs>;`.
    /// A declaration whose value is `rhs`, wrapped or not: rustfmt puts a value
    /// too wide for the line under the `=`, and a scan that could not see one
    /// would pass by finding nothing to hold.
    fn declares(source: &str, rhs: &str) -> bool {
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
            ("bootloader/src/loaderlog.rs", format!("cstr16!(\"{LOADER_LOG}\")")),
            ("bootloader/src/loaderlog.rs", format!("\"{LOADER_FIRST_LINE}\"")),
            ("bootloader/src/loaderlog.rs", format!("\"{LOADER_LAST_LINE}\"")),
            ("bootloader/src/loaderlog.rs", format!("\"{CHAIN_ENDS_LINE}\"")),
            ("bootloader/src/loaderlog.rs", format!("\"{SEPARATOR}\"")),
            ("bootloader/src/main.rs", format!("\"{HUNG_WITHOUT_A_RECORD}\"")),
            ("bootloader/src/loaderlog.rs", format!("\"{LOADER_GOP_LINE}\"")),
            ("bootloader/src/blackbox.rs", format!("\"{BLACKBOX_HEAD}\"")),
            ("bootloader/src/blackbox.rs", format!("\"{PREVIOUS_PANIC}\"")),
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
    }

    #[test]
    fn the_loaders_file_is_told_from_logds_however_a_driver_spelled_it() {
        let listing = "2026-09-06-084003.log\nloader.log\nunknown-00.log\nnotes.txt\n";
        assert_eq!(
            split_listing(listing),
            (Some("loader.log"), vec!["2026-09-06-084003.log", "unknown-00.log"])
        );
        // A FAT driver that drops the lowercase flags yields 8.3 in upper case.
        assert_eq!(split_listing("LOADER.LOG\n").0, Some("LOADER.LOG"));
        // And it is never one of logd's, under either spelling.
        assert!(split_listing("LOADER.LOG\nloader.log\n").1.is_empty());
        // Blank rows and stray whitespace are a listing's, not a name's.
        assert_eq!(split_listing("\n  loader.log  \n\n").0, Some("loader.log"));
        assert_eq!(split_listing(""), (None, Vec::new()));
        // Somebody else's file, which nothing here may name or delete.
        assert_eq!(split_listing("boot.log\n"), (None, Vec::new()));
    }

    /// The two kernel spellings a metal readback is judged on, and the length
    /// it truncates a name to — held to the kernel's own source, because
    /// nothing links this crate to it either.
    #[test]
    fn the_kernel_writes_the_records_the_host_reads() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (file, needle) in [
            ("kernel/src/process.rs", format!("log!(\"{EXIT}{{name}} pid=")),
            ("kernel/src/arch/smp.rs", format!("log!(\"{AP_BRINGUP}")),
            ("kernel/src/process.rs", format!("THREAD_NAME_LEN: usize = {NAME_LEN}")),
            ("kernel/src/deadline.rs", format!("EXPIRED: &str = \"{DEADLINE_EXPIRED}\"")),
            ("kernel/src/deadline.rs", format!("WEDGE_STAGED: &str = \"{WEDGE_STAGED}\"")),
            ("kernel/src/deadline.rs", format!("\"{WEDGE_ARRIVED_DEAF}\"")),
            ("kernel/src/hardlockup/mod.rs", format!("LOCKED_UP: &str = \"{LOCKED_UP}\"")),
            (
                "kernel/src/hardlockup/probe.rs",
                format!("PROBE_STAGED: &str = \"{LOCKUP_STAGED}\""),
            ),
            ("kernel/src/log/mod.rs", format!("\"{LOG_COMPLETE}")),
            ("kernel/src/log/mod.rs", format!("\"{LOG_SHORT}")),
            ("kernel/src/log/mod.rs", format!("\"{LOG_TAIL}")),
        ] {
            let at = root.join(file);
            let source = std::fs::read_to_string(&at).expect("a kernel module");
            assert!(source.contains(&needle), "{} does not write {needle:?}", at.display());
        }
    }

    /// The truncation, which is what a whole-name predicate would miss.
    #[test]
    fn a_long_binarys_record_is_the_prefix_the_kernel_keeps() {
        assert_eq!(recorded_name("test_rs_mkdir_cap"), "test_rs_mkdir_cap");
        assert_eq!(recorded_name("test_rs_null_sink_client_exits"), "test_rs_null_sink_client_ex");
        assert_eq!(recorded_name("/system/bin/echo"), "echo");
        assert_eq!(recorded_name("").len(), 0);
    }

    #[test]
    fn the_boot_record_is_the_kernels_own_line() {
        assert_eq!(boot_millis("[kernel 1.151 cpu0] Boot: complete (1151ms)\n"), Some(1151));
        assert_eq!(boot_millis("[kernel 0.084 cpu0] Boot: storage ready (84ms)\n"), None);
        assert_eq!(boot_millis("Boot: complete (later)\n"), None);
        assert_eq!(boot_millis(""), None);
    }
}

#[cfg(test)]
mod record_time_tests {
    use super::*;

    /// Both writers' shapes: `logd`'s file carries a wall-clock tag before the
    /// elapsed field and the panel carries none, and the same reader answers
    /// for both.
    #[test]
    fn a_records_elapsed_field_is_read_past_whatever_tag_precedes_it() {
        assert_eq!(
            record_millis("[2026-09-07 22:57:46 3.109 cpu1] exit: a pid=7 code=0 cpu=180ms"),
            Some(3_109)
        );
        assert_eq!(record_millis("[3.109 cpu1] exit: a pid=7 code=0"), Some(3_109));
        assert_eq!(
            record_millis("[2026-09-07 22:58:03 20.071 cpu2 tid=1] exit: b tid=1 code=0"),
            Some(20_071)
        );
        // The `cpu=180ms` field is a record's *content*: a reader keying on the
        // first `cpu` would answer with a duration instead of a timestamp.
        assert_eq!(record_millis("no timestamp here, cpu=1ms"), None);
        assert_eq!(record_millis(""), None);
    }

    #[test]
    fn the_last_record_is_the_last_line_that_carries_a_time() {
        let log = "[1.000 cpu0] first\n[2.500 cpu1] second\nnot a record\n";
        assert_eq!(last_record_millis(log), Some(2_500));
        assert_eq!(last_record_millis("nothing\n"), None);
    }

    /// One boot's records, verbatim from a stick the T14 wrote
    /// (`lancase-run31/kernel.log` lines 1, 279 and 379).
    const BOOT: &str = concat!(
        "[2026-09-08 16:08:21 0.000 cpu0 boot] panic console: armed 1920x1080 stride=1920 \
         format=1 at 0x4000000000\n",
        "[2026-09-08 16:08:22 1.258 cpu0] Boot: complete (1258ms)\n",
        "[2026-09-08 16:08:44 23.340 cpu1] Rebooting.\n",
    );

    /// The whole window a host watches the machine over, wider than the boot at
    /// both ends because firmware runs inside it.
    fn window() -> (u64, u64) {
        let (first, ended) = record_unix_span(BOOT).expect("a span");
        (first - 4, ended + 36)
    }

    #[test]
    fn a_second_inside_the_boot_and_after_the_named_record_is_this_boots() {
        let (first, ended) = record_unix_span(BOOT).expect("a span");
        assert_eq!(ended - first, 23);
        assert_eq!(
            host_second_inside_this_boot(BOOT, window(), "Boot: complete", first + 2),
            Ok(())
        );
    }

    /// **A reply after the boot handed the machine back is the next operating
    /// system's, however early in the host's window it fell.** The window opens
    /// no later than the boot's first record, so a reply 57 s into it came at
    /// least 34 s after this boot's `Rebooting.`
    #[test]
    fn a_second_past_the_reboot_record_is_the_next_operating_systems() {
        let (first, ended) = record_unix_span(BOOT).expect("a span");
        let why = host_second_inside_this_boot(BOOT, window(), "Boot: complete", first + 57)
            .expect_err("57 s past the window's opening is past this boot");
        assert!(why.contains(&format!("{first}..{ended}")), "{why}");
        assert!(why.contains("34 s after the boot"), "{why}");
    }

    #[test]
    fn a_second_before_the_named_record_is_refused_by_that_record() {
        let (first, _) = record_unix_span(BOOT).expect("a span");
        let why = host_second_inside_this_boot(BOOT, window(), "Boot: complete", first)
            .expect_err("the boot had not completed yet");
        assert!(why.contains("1 s before"), "{why}");
        let why = host_second_inside_this_boot(BOOT, window(), "netd: DHCP: lease ", first + 2)
            .expect_err("this boot took no lease");
        assert!(why.contains("no \"netd: DHCP: lease \" record"), "{why}");
    }

    /// **The window is what bounds the two clocks' disagreement.** A boot whose
    /// records fall outside the span the host watched it over is a boot whose
    /// clock cannot be held against the host's at all, and the numbers are
    /// printed rather than the conclusion.
    #[test]
    fn records_outside_the_hosts_own_window_place_nothing() {
        let (first, ended) = record_unix_span(BOOT).expect("a span");
        let why = host_second_inside_this_boot(BOOT, (first + 5, ended + 36), "Rebooting.", ended)
            .expect_err("the boot began before the host started watching");
        assert!(why.contains(&format!("{first}..{ended}")), "{why}");
        assert!(why.contains("disagree by more than the window"), "{why}");
    }

    /// The panel writes no wall clock, and its milliseconds field must not be
    /// read as one: `[1.000 cpu0]` would otherwise parse `1.000` as a date and
    /// answer some second in 1970.
    #[test]
    fn a_line_with_no_wall_clock_answers_none() {
        assert_eq!(record_unix_secs("[1.000 cpu0] first"), None);
        assert_eq!(record_unix_secs("not a record"), None);
        assert_eq!(record_unix_secs("[2026-09-08 25:00:00 0.000 cpu0] x"), None);
        assert_eq!(record_unix_secs("[2026-02-31 10:00:00 0.000 cpu0] x"), None);
        assert_eq!(record_unix_span("[1.000 cpu0] first\n"), None);
        let why = host_second_inside_this_boot("[1.000 cpu0] first\n", (0, 1), "x", 0)
            .expect_err("a panel log carries no wall clock");
        assert!(why.contains("no record with a wall clock"), "{why}");
    }
}
