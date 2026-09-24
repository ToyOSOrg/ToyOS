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

/// What the USB wedge arms say before the write they stop the machine inside,
/// in `kernel/src/usb_gate.rs`; the phase and the traffic behind it follow on
/// the same line.
///
/// The witness that the boot the deadline then ended was one holding a device
/// inside a Bulk-Only command, which is the whole of what those controls stage —
/// a wedge taken anywhere else is `WEDGE_STAGED`'s boot with a longer log.
pub const USB_WEDGE_STAGED: &str = "usb-wedge: stopping every CPU at the";

/// What the same arms say if every write ran to completion, which means no CPU
/// was stopped inside one.
///
/// **A control that stages nothing passes for the wrong reason**: without this
/// line the boot would still wedge — at the shutdown, with no device inside
/// anything — and read back exactly like the arm that proves the point.
pub const USB_WEDGE_MISSED: &str = "usb-wedge: the write completed";

/// What the `usb-reset-under-load` arm says once it is streaming, and the three
/// ways it says it is not, in `kernel/src/usb_gate.rs`.
///
/// **The witness that the reset landed on a busy bus.** A boot whose page
/// carries the first line and none of the other three is one whose reset found
/// a controller still moving bytes; any of the other three is the idle case
/// under this arm's name.
pub const USB_LOAD_RUNNING: &str = "usb-load: sweeping disk 0";
pub const USB_LOAD_REFUSED: &str = "usb-load: refused";
pub const USB_LOAD_STOPPED: &str = "usb-load: the disk stopped answering";
pub const USB_LOAD_SWEPT: &str = "usb-load: the sweep reached the end of the disk";

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

/// What the loader prints in place of a record's tail, with the count of the
/// records it filed instead.
pub const TAIL_IN_THE_FILE: &str =
    "Black box: that record's log ring is in loader.log, not on a console the firmware scrolls:";

/// What the loader closes the one line of a record the page's end cut with.
///
/// **The loader re-terminates every line it prints off the page**, so this is
/// the only mark a cut leaves: without it a field read off such a line is a
/// number the page cut rather than the one the kernel wrote.
pub const CUT_BY_THE_PAGE: &str = " <the page ended here, mid-line>";

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

/// What the on-screen panel cost the boot, in
/// `kernel/src/drivers/panic_console/mod.rs`.
///
/// **Two channels carry it**, because the panel is the window a boot that ends
/// in a wedge still has: the shutdown census reaches `logd`'s file, and the
/// same line is sealed into the black-box page for a boot no `logd` outlived.
pub const PANEL_CENSUS: &str = "panel: paints=";

/// What one boot's panel census says: how often the panel painted, how many
/// pixels it put on the glass, how long it spent inside the painter, and the
/// slowest single paint of the boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Panel {
    pub paints: u64,
    pub pixels: u64,
    pub micros: u64,
    pub max_micros: u64,
}

/// The census `log` carries, or `None` for a boot that left neither channel.
///
/// **This boot's census whole, or nothing.** A `max_us=` whose digits a cut
/// took the end of parses as a cheaper panel than the boot had, and each
/// channel says a line ended itself in its own way: `logd`'s file terminates
/// one, and the black-box page is a fixed size whose one cut line the loader
/// closes with [`CUT_BY_THE_PAGE`]. An older census standing in for a cut one
/// would be a second boot's number under this boot's name, so the cut line is
/// refused rather than skipped.
pub fn panel_census(log: &str) -> Option<Panel> {
    let line =
        log.split_inclusive('\n').rev().find(|line| !is_spoken(line) && line.contains(PANEL_CENSUS))?;
    if !line.ends_with('\n') || line.contains(CUT_BY_THE_PAGE) {
        return None;
    }
    let field = |name: &str| -> Option<u64> {
        let (_, rest) = line.split_once(name)?;
        rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    Some(Panel {
        paints: field("paints=")?,
        pixels: field("px=")?,
        micros: field(" us=")?,
        max_micros: field("max_us=")?,
    })
}

/// The kernel's record for a process that ended, in `kernel/src/process.rs`.
///
/// **The one channel a guest binary's verdict crosses on a machine with no
/// serial port that no program's share of the log bounds**: its own output
/// reaches the stick only as records inside its share, and this is the kernel's
/// record, in a form no program's record takes ([`SPOKEN`]).
pub const EXIT: &str = "exit: ";

/// The kernel's record for a process that started, in `kernel/src/process.rs`.
///
/// Read for where it must *not* be: after the boot's own last word, where it
/// says a process still on a run queue started another one under a shutdown.
pub const SPAWN: &str = "spawn: ";

/// What opens every program's record and no record of the kernel's own:
/// `toyos_elide::spoken::SIGIL`, then the program's tag and `": "`.
pub const SPOKEN: char = '@';

/// One rendered record's message: what follows the bracket every writer opens
/// a line with. `None` for a line that is not a record's first.
pub fn message(line: &str) -> Option<&str> {
    line.strip_prefix('[')?.split_once("] ").map(|(_, message)| message)
}

/// Whether a rendered line is a program's record.
pub fn is_spoken(line: &str) -> bool {
    message(line).is_some_and(|m| m.starts_with(SPOKEN))
}

/// A log without its programs' records: what a judge of the kernel's own reads.
pub fn kernel_records(log: &str) -> String {
    log.split_inclusive('\n').filter(|line| !is_spoken(line)).collect()
}

/// One program's records, by the tag the kernel gives it (`name` as spawned).
pub fn records_of(log: &str, name: &str) -> String {
    let head = format!("{SPOKEN}{name}: ");
    log.split_inclusive('\n').filter(|l| message(l).is_some_and(|m| m.starts_with(&head))).collect()
}

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
/// `logd` writes the wall clock and the panel writes none, so a line without one
/// answers `None` rather than reading the milliseconds field as a date.
fn record_unix_secs(line: &str) -> Option<u64> {
    let mut fields = line.strip_prefix('[')?.split_whitespace();
    let (year, rest) = fields.next()?.split_once('-')?;
    let (month, day) = rest.split_once('-')?;
    let (hour, rest) = fields.next()?.split_once(':')?;
    let (min, sec) = rest.split_once(':')?;
    if [year, month, day, hour, min, sec].map(str::len) != [4, 2, 2, 2, 2, 2] {
        return None;
    }
    let civil = toyos_wallclock::Civil {
        year: year.parse().ok()?,
        month: month.parse().ok()?,
        day: day.parse().ok()?,
        hour: hour.parse().ok()?,
        min: min.parse().ok()?,
        sec: sec.parse().ok()?,
    };
    // logd renders this field from the same `Civil`, so one it refuses is not
    // a field logd wrote.
    civil.is_valid().then(|| civil.to_unix_secs())
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

/// The daylight a host second needs on either side before it is this boot's.
///
/// **A judge reading whole seconds does not get to decide at one.** `skew` is a
/// difference of two floored clocks across a round trip its reader holds to a
/// second, which is three of these; the second the host read and the second the
/// record carries are floored too, which is the fourth; and the fifth is what
/// makes a refusal a distance rather than a coin.
pub const MARGIN: u64 = 5;

/// Whether a second on the *host's* clock fell inside the boot this log is of,
/// clear of [`MARGIN`] on both the record `after` names and the reset.
///
/// **The records are the one place a host clock and a boot's clock meet.**
/// `skew` is this machine's clock minus the host's as the caller measured the
/// two against each other; how far into a host-side window an observation came
/// separates nothing, because such a window holds the operating system that
/// left and the one that came back as well as this boot.
pub fn host_second_inside_this_boot(
    log: &str,
    skew: i64,
    after: &str,
    at: u64,
) -> Result<(), String> {
    let dated = |line: Option<&str>| line.and_then(record_unix_secs).map(i128::from);
    let began = dated(log.lines().find(|l| l.contains(after)))
        .ok_or_else(|| format!("this log carries no dated {after:?} record"))?;
    let ended = dated(log.lines().rfind(|l| !is_spoken(l) && l.contains(REBOOTING))).ok_or_else(|| {
        format!(
            "this log carries no dated {REBOOTING:?} record, so nothing in it says when this boot \
             handed the machine back"
        )
    })?;
    let at = i128::from(
        at.checked_add_signed(skew)
            .ok_or_else(|| format!("a host second of {at} and a skew of {skew} is no second"))?,
    );
    if at - began < i128::from(MARGIN) {
        return Err(format!(
            "the host saw it at {at} on this machine's clock and this boot's {after:?} record is \
             at {began}, {} s apart: nothing closer than {MARGIN} s past that record is this \
             boot's, because these clocks are whole seconds",
            at - began
        ));
    }
    if ended - at < i128::from(MARGIN) {
        return Err(format!(
            "the host saw it at {at} on this machine's clock and this boot's {REBOOTING:?} record \
             is at {ended}, {} s apart: nothing closer than {MARGIN} s before that record is this \
             boot's, so it belongs to the operating system on the other side of the reset",
            ended - at
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
    let tail = log.lines().filter(|l| !is_spoken(l)).find_map(|line| line.split(COMPLETE).nth(1))?;
    tail.split("ms)").next()?.parse().ok()
}

/// A boot's duration if its log is a passing boot's, which takes both records:
/// a log ending anywhere but the reset is a machine that did not come back on
/// its own, so the word is looked for as the last line and not in the text.
pub fn verdict(log: &str) -> Result<u64, Unfit> {
    let boot_ms = boot_millis(log).ok_or(Unfit::NoBootRecord)?;
    let last = log
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !is_spoken(line))
        .unwrap_or_default();
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
            ("bootloader/src/blackbox.rs", format!("\"{TAIL_IN_THE_FILE}\"")),
            ("bootloader/src/blackbox.rs", format!("\"{CUT_BY_THE_PAGE}\"")),
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
            ("kernel/src/usb_gate.rs", format!("USB_WEDGE_STAGED: &str = \"{USB_WEDGE_STAGED}\"")),
            ("kernel/src/usb_gate.rs", format!("USB_WEDGE_MISSED: &str = \"{USB_WEDGE_MISSED}\"")),
            ("kernel/src/usb_gate.rs", format!("LOAD_RUNNING: &str = \"{USB_LOAD_RUNNING}\"")),
            ("kernel/src/usb_gate.rs", format!("LOAD_REFUSED: &str = \"{USB_LOAD_REFUSED}\"")),
            ("kernel/src/usb_gate.rs", format!("LOAD_STOPPED: &str = \"{USB_LOAD_STOPPED}\"")),
            ("kernel/src/usb_gate.rs", format!("LOAD_SWEPT: &str = \"{USB_LOAD_SWEPT}\"")),
            ("kernel/src/hardlockup/mod.rs", format!("LOCKED_UP: &str = \"{LOCKED_UP}\"")),
            (
                "kernel/src/hardlockup/probe.rs",
                format!("PROBE_STAGED: &str = \"{LOCKUP_STAGED}\""),
            ),
            (
                "kernel/src/drivers/panic_console/mod.rs",
                format!("CENSUS: &str = \"{PANEL_CENSUS}\""),
            ),
            ("kernel/src/log/mod.rs", format!("\"{LOG_COMPLETE}")),
            ("kernel/src/log/mod.rs", format!("\"{LOG_SHORT}")),
            ("kernel/src/log/mod.rs", format!("\"{LOG_TAIL}")),
            ("toyos-elide/src/spoken.rs", format!("SIGIL: u8 = b'{SPOKEN}';")),
        ] {
            let at = root.join(file);
            let source = std::fs::read_to_string(&at).expect("a kernel module");
            assert!(source.contains(&needle), "{} does not write {needle:?}", at.display());
        }
    }

    /// A program spawned as `exit` writing a kernel verdict's words is left out of
    /// the kernel's records, and a kernel record's continuation line is kept.
    #[test]
    fn a_programs_record_is_never_the_kernels() {
        let log = "[2026-09-08 16:08:23 2.100 cpu0] exit: test_rs_job pid=4 code=3 cpu=1ms\n\
                   [2026-09-08 16:08:23 2.150 cpu0] PANIC: a report\n  its second line\n\
                   [2026-09-08 16:08:23 2.200 cpu1] @exit: test_rs_job pid=4 code=0 cpu=0ms\n\
                   [2026-09-08 16:08:23 2.300 cpu1] @evil: exit: test_rs_job pid=4 code=0\n";
        let kernel = kernel_records(log);
        assert!(!kernel.contains("code=0"), "{kernel}");
        assert!(kernel.contains("code=3") && kernel.contains("  its second line\n"), "{kernel}");
        assert_eq!(records_of(log, "exit").lines().count(), 1);
        assert_eq!(records_of(log, "evil").lines().count(), 1);
        assert!(is_spoken(log.lines().nth(3).expect("four lines")));
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

    /// The two channels the census crosses, read by one reader: `logd`'s file,
    /// and the black-box page the loader prints back with its own margin and
    /// with the kernel's dashes flattened to ASCII.
    #[test]
    fn the_panel_census_is_read_off_either_channel() {
        let logd = "[2026-09-08 06:50:53 2.5 cpu0] panel: paints=3 px=6220800 us=1500000 \
                    max_us=520000\n";
        assert_eq!(
            panel_census(logd),
            Some(Panel { paints: 3, pixels: 6_220_800, micros: 1_500_000, max_micros: 520_000 })
        );

        // A wedge boot's page as the pass after the reset prints it back, line
        // for line off run 44's deadlinewedge `loader.log`: head lines, the
        // count of what went to the file, and the filed records under it. Only
        // the page's last line can be cut, and `cut` is where it fell.
        let page = |census: &str, cut: &str| {
            format!(
                "Previous boot's panic: the last boot read WEDGED, so a bound of its own ended \
                 it and this chain ends here\n\
                 | the boot deadline expired: a bound of 120000 ms, reached at 120066 ms, with \
                 this machine in `complete`. The tail of the log ring follows ... which is what \
                 nothing was draining.\n\
                 | panel: {census}\n\
                 | older records dropped to fit this page: 84\n\
                 | usb-quiesce: no barrier was taken, so this reset is not the shutdown's\n\
                 | usb-quiesce: no bulk transfer was outstan{cut}\n\
                 {TAIL_IN_THE_FILE} 221 record(s)\n\
                 | [0.148 cpu0] iommu: unit3 scope ioapic 00:1e.7 id=2\n"
            )
        };
        // Run 44's own reading, with the cut two lines under the census.
        assert_eq!(
            panel_census(&page("paints=10 px=8886656 us=18693 max_us=3867", CUT_BY_THE_PAGE)),
            Some(Panel { paints: 10, pixels: 8_886_656, micros: 18_693, max_micros: 3_867 })
        );
        // The same page with the cut inside the census's last number instead —
        // a 38 us panel, and every line under it still terminated.
        let mid_number = format!("paints=10 px=8886656 us=18693 max_us=38{CUT_BY_THE_PAGE}");
        assert_eq!(panel_census(&page(&mid_number, "")), None);

        // `logd`'s file ends a record with the newline, so its own cut is a
        // last line that never got one.
        assert_eq!(panel_census("panel: paints=3 px=6220800 us=1500000 max_us=52"), None);
        // A field the cut took whole, and a log with no census at all.
        assert_eq!(panel_census("panel: paints=3 px=6220800 us=1500000\n"), None);
        assert_eq!(panel_census("nothing here\n"), None);
    }

    #[test]
    fn the_last_record_is_the_last_line_that_carries_a_time() {
        let log = "[1.000 cpu0] first\n[2.500 cpu1] second\nnot a record\n";
        assert_eq!(last_record_millis(log), Some(2_500));
        assert_eq!(last_record_millis("nothing\n"), None);
    }

    const BOOT: &str = concat!(
        "[2026-09-08 16:08:21 0.000 cpu0 boot] panic console: armed 1920x1080 stride=1920 \
         format=1 at 0x4000000000\n",
        "[2026-09-08 16:08:22 1.258 cpu0] Boot: complete (1258ms)\n",
        "[2026-09-08 16:08:44 23.340 cpu1] Rebooting.\n",
    );

    /// That boot's first record, which every second below is placed against.
    fn first() -> u64 {
        record_unix_secs(BOOT.lines().next().expect("a record")).expect("a wall clock")
    }

    /// **[`MARGIN`] decides both edges**, and one second short of either is
    /// refused rather than read as inside.
    #[test]
    fn a_second_clear_of_this_boots_records_by_the_margin_is_this_boots() {
        let first = first();
        for at in [first + MARGIN + 1, first + 23 - MARGIN] {
            assert_eq!(host_second_inside_this_boot(BOOT, 0, "Boot: complete", at), Ok(()), "{at}");
        }
        let why = host_second_inside_this_boot(BOOT, 0, "Boot: complete", first + MARGIN)
            .expect_err("a second short of the margin past the record it is anchored on");
        assert!(why.contains(&format!("closer than {MARGIN} s past")), "{why}");
        let why = host_second_inside_this_boot(BOOT, 0, "Boot: complete", first + 24 - MARGIN)
            .expect_err("a second short of the margin before the reset");
        assert!(why.contains(&format!("closer than {MARGIN} s before")), "{why}");
    }

    /// **The one reply this judge exists to refuse.** The loop wrote it 57 s
    /// into a window opening no earlier than its own run, whose first line is
    /// 33 s before this boot's first record; it is anchored here on the earliest
    /// record the boot carries, which is the most favourable anchor there is,
    /// and no skew the measurement can be wrong by brings it inside.
    #[test]
    fn that_reply_is_refused_at_every_skew_the_measurement_can_be_wrong_by() {
        let earliest_window = first() - 33;
        for skew in -3..=3 {
            let why =
                host_second_inside_this_boot(BOOT, skew, "Boot: complete", earliest_window + 57)
                    .expect_err("a skew of this size does not place that reply inside the boot");
            assert!(why.contains(&format!("closer than {MARGIN} s before")), "{skew}: {why}");
        }
    }

    /// **A boot that never reached its reset brackets nothing**, and neither
    /// does one that never wrote the record the caller anchors on.
    #[test]
    fn a_log_missing_either_record_is_refused_rather_than_widened() {
        let first = first();
        let unfinished: String = BOOT.lines().take(2).map(|l| format!("{l}\n")).collect();
        let why = host_second_inside_this_boot(&unfinished, 0, "Boot: complete", first + 10)
            .expect_err("a log with no reset says nothing about when this boot ended");
        assert!(why.contains(&format!("no dated {REBOOTING:?} record")), "{why}");
        let why = host_second_inside_this_boot(BOOT, 0, "netd: DHCP: lease ", first + 10)
            .expect_err("this boot took no lease");
        assert!(why.contains("no dated \"netd: DHCP: lease \" record"), "{why}");
        let why = host_second_inside_this_boot("[1.000 cpu0] first\n", 0, "first", 0)
            .expect_err("a panel log carries no wall clock");
        assert!(why.contains("no dated \"first\" record"), "{why}");
    }

    /// **The measured skew is the whole of what places a host second.** The
    /// same reading is this boot's on one clock and the next operating system's
    /// on another.
    #[test]
    fn the_measured_skew_is_what_the_host_second_is_read_through() {
        let first = first();
        assert_eq!(host_second_inside_this_boot(BOOT, 30, "Boot: complete", first - 20), Ok(()));
        let why = host_second_inside_this_boot(BOOT, -30, "Boot: complete", first + 10)
            .expect_err("thirty seconds the other way is before this boot began");
        assert!(why.contains(&format!("closer than {MARGIN} s past")), "{why}");
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
    }

    /// A leap day and a month's end are counted as the days they are.
    #[test]
    fn a_wall_clock_across_a_leap_day_and_a_month_is_its_seconds() {
        let at = |line| record_unix_secs(line).expect("a wall clock");
        assert_eq!(at("[1970-01-01 00:00:00 0.000 cpu0] x"), 0);
        assert_eq!(at("[2024-02-28 23:59:59 0.000 cpu0] x"), 1_709_164_799);
        assert_eq!(at("[2024-02-29 00:00:00 0.000 cpu0] x"), 1_709_164_800);
        assert_eq!(at("[2024-03-01 00:00:00 0.000 cpu0] x"), 1_709_164_800 + 86_400);
        assert_eq!(record_unix_secs("[2023-02-29 00:00:00 0.000 cpu0] x"), None);
    }
}
