//! A program's console lines as the kernel's records: tagged with the program's
//! name, in `/log`, on the panel, and bounded per program.
//!
//! **The machine these are about has no serial port.** On the T14 a console
//! object's serial backend is `Backend::None`, so a line a program writes
//! reaches the development host only as a record — through `logd`'s `/log`,
//! the log stream, or the panel while no compositor holds the screen. The
//! muted metal profile is that machine: every verdict below that is about
//! reaching the owner is read off the volume or the glass, never a console.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::qemu::{self, BootOptions, QemuInstance};
use super::volumes::{log_extent, newest_log};

/// The line `test-runner` writes first, as its record carries it: in the form
/// the kernel gives a program's record, under the name it was spawned as.
const RUNNER_READY: &str = "@test-runner: ===READY===";

/// A line `init` writes naming itself, which the kernel does not name twice.
const INIT_STARTED_LOGD: &str = "@init: started logd";

/// The flooding program's name as its records carry it, and the prefix of
/// each of its numbered lines.
const FLOODER: &str = "test_rs_console_flood";
const FLOOD_LINE: &str = "flood ";
const FLOOD_LINES: usize = 4000;

/// What the kernel says, in the flooder's name, about the lines it did not record.
const WITHHELD: &str = " line(s) past this program's share of the log were not recorded]";

fn image_at(name: &str) -> PathBuf {
    super::lane::dir().join(name)
}

/// One `/log` line as `logd` renders it: the monotonic milliseconds of the
/// record and its message. `None` for a line that is not a record.
fn record(line: &str) -> Option<(u64, &str)> {
    let inside = line.strip_prefix('[')?;
    let (bracket, message) = inside.split_once("] ")?;
    let words: Vec<&str> = bracket.split_whitespace().collect();
    let at = words.iter().position(|w| w.starts_with("cpu"))?.checked_sub(1)?;
    let (secs, millis) = words.get(at)?.split_once('.')?;
    Some((secs.parse::<u64>().ok()? * 1000 + millis.parse::<u64>().ok()?, message))
}

/// Poll the volume until this boot's log carries a record whose message is
/// `want`, or `bound` passes; the log text either way.
fn await_record(image: &Path, start: usize, len: usize, want: &str, bound: Duration) -> String {
    let deadline = Instant::now() + bound;
    loop {
        let text = newest_log(image, start, len)
            .map(|(_, bytes)| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        if text.lines().any(|l| record(l).is_some_and(|(_, m)| m == want)) || Instant::now() >= deadline
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A program's line on the machine with no serial port: a record tagged with
/// its name in `/log`, and a row on the panel, with no compositor running.
pub fn console_line_is_a_record(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let image_path = image_at("console-line-record.img");
    let image = qemu::build_boot_image(test_config, c_bins, rust_bins, &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = log_extent(&image, &image_path)?;

    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        mute: true,
        boot_image: Some(qemu::Staged::Written(image_path.clone())),
        ..Default::default()
    };
    let argv = qemu::profile_argv(&options);
    match argv.iter().position(|a| a == "-serial") {
        Some(i) if argv.get(i + 1).is_some_and(|v| v == "none") => {}
        _ => return Err(format!("the muted profile still has a 16550: {argv:?}")),
    }
    let mut guest = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);

    // The panel first: the one console a T14 owner sees before anything else.
    let dump = guest.screendump_until(RUNNER_READY, Duration::from_secs(30));
    let screen = dump.text();
    if !screen.contains(RUNNER_READY) {
        return Err(format!(
            "no {RUNNER_READY:?} on the panel of a guest with no serial port and no \
             compositor\ndecoded screen:\n{screen}"
        ));
    }

    let log = await_record(&image_path, start, len, RUNNER_READY, Duration::from_secs(10));
    drop(guest);
    let messages: Vec<&str> = log.lines().filter_map(record).map(|(_, m)| m).collect();
    if !messages.contains(&RUNNER_READY) {
        return Err(format!(
            "/log carries no record {RUNNER_READY:?}: the runner's line reached the panel and \
             not the file\n{log}"
        ));
    }
    if !messages.contains(&INIT_STARTED_LOGD) {
        return Err(format!("/log carries no record {INIT_STARTED_LOGD:?}\n{log}"));
    }
    if let Some(twice) = messages.iter().find(|m| m.starts_with("@init: init: ")) {
        return Err(format!("init named itself and the kernel named it again: {twice:?}"));
    }
    eprintln!("  [spoken] on the panel and in /log, with no serial port: {RUNNER_READY:?}");
    Ok(())
}

/// A program that writes far past its share: the serial console keeps every
/// line, the ring keeps its share and a count of the rest, and the kernel's own
/// records around the flood reach `/log` with no hole in front of them.
pub fn console_flood_is_bounded(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let image_path = image_at("console-flood.img");
    let image = qemu::build_boot_image(test_config, c_bins, rust_bins, &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = log_extent(&image, &image_path)?;

    let mut guest = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            boot_image: Some(qemu::Staged::Written(image_path.clone())),
            ..Default::default()
        },
    );
    let ran = guest.run_test(FLOODER, Duration::from_secs(120));
    if ran.exit_code != Some(0) {
        return Err(format!("{FLOODER} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    let done = format!("flood done lines={FLOOD_LINES}");
    // **The serial console is unchanged**: every line once, raw, and none of
    // them a second time as a drained record.
    let mut seen = vec![0usize; FLOOD_LINES];
    for line in ran.stdout.lines().chain(ran.serial.lines()) {
        if line.contains(&format!("] @{FLOODER}: ")) {
            return Err(format!("a spoken record was drained onto the serial console: {line:?}"));
        }
    }
    for line in ran.stdout.lines() {
        if let Some(n) = line.trim().strip_prefix(FLOOD_LINE).and_then(|n| n.parse::<usize>().ok()) {
            if let Some(count) = seen.get_mut(n) {
                *count += 1;
            }
        }
    }
    if let Some(n) = seen.iter().position(|&c| c != 1) {
        return Err(format!(
            "line {n} reached the serial console {} time(s); every one of {FLOOD_LINES} must, once",
            seen[n]
        ));
    }
    if !ran.stdout.lines().any(|l| l.trim() == done) {
        return Err(format!("the flooder never said {done:?}\n{}", ran.stdout));
    }

    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU: {e}"))?;
    guest.flush_stdin();
    let tail = guest.drain_serial(Duration::from_secs(20));
    drop(guest);
    let (_, bytes) = newest_log(&image_path, start, len)?;
    let log = String::from_utf8_lossy(&bytes).into_owned();

    for said in [ran.before.as_str(), ran.stdout.as_str(), ran.serial.as_str(), tail.as_str(), &log] {
        if let Some(line) = said.lines().find(|l| l.contains("were overwritten in a shard")) {
            return Err(format!("the kernel's records were lapped under the flood: {line}"));
        }
    }

    let own = format!("@{FLOODER}: ");
    let mut recorded: Vec<u64> = Vec::new();
    let mut counted = 0u64;
    let mut stated = false;
    for (at, message) in log.lines().filter_map(record) {
        let Some(said) = message.strip_prefix(&own) else { continue };
        if said.starts_with(FLOOD_LINE) {
            recorded.push(at);
        } else if let Some(count) = said.strip_prefix("...[").and_then(|s| s.strip_suffix(WITHHELD))
        {
            counted += count.parse::<u64>().map_err(|_| format!("an unreadable count: {message:?}"))?;
            stated = true;
        }
    }
    let lines = FLOOD_LINES as u64 + 1;
    if recorded.len() as u64 + counted != lines {
        return Err(format!(
            "{} of the flooder's {lines} lines were recorded and {counted} counted: a line went \
             missing from both",
            recorded.len()
        ));
    }
    if !stated {
        return Err(format!(
            "all {lines} of the flooder's lines were recorded: no program's share bounds the ring"
        ));
    }
    // The share, as `kernel/src/log/spoken.rs` declares it: a burst, then a rate.
    const BURST: u64 = 128;
    const PER_SEC: u64 = 16;
    let span_ms = recorded.last().zip(recorded.first()).map_or(0, |(last, first)| last - first);
    // A millisecond for the rendering rounding each stamp down, and one line for
    // the share being charged on a clock read just before the record is stamped,
    // with preemption off under the console's lock between the two.
    let bound = BURST + (span_ms + 1) * PER_SEC / 1000 + 1;
    // Non-vacuity: the paced half is what the rate admits, so a run that recorded
    // only the burst never measured the rate.
    if recorded.len() as u64 <= BURST {
        return Err(format!(
            "{} of the flooder's lines were recorded, none past the burst: the paced half \
             never reached the rate",
            recorded.len()
        ));
    }
    if recorded.len() as u64 > bound {
        return Err(format!(
            "{} of the flooder's lines were recorded over {span_ms} ms, and its share is {bound}",
            recorded.len()
        ));
    }

    // The kernel's records on either side of the flood are in the file.
    let messages: Vec<&str> = log.lines().filter_map(record).map(|(_, m)| m).collect();
    let spawned = format!("spawn: /system/bin/{FLOODER} ");
    let exited = format!("exit: {FLOODER} ");
    for want in [spawned.as_str(), exited.as_str()] {
        if !messages.iter().any(|m| m.starts_with(want)) {
            return Err(format!("/log lost the kernel's own record {want:?} around the flood"));
        }
    }
    eprintln!(
        "  [spoken] {} of {lines} flood lines recorded over {span_ms} ms (share {bound}), {counted} \
         counted, every one on serial once, and the kernel's records around it kept",
        recorded.len()
    );
    Ok(())
}

/// The job whose verdict a program tries to forge, and the exit it really has.
const FORGER: &str = "test_rs_console_forger";
const FORGER_CODE: i64 = 7;

/// What the copy named `exit` writes: the kernel's `exit:` record's words,
/// claiming the job passed.
const FORGED: &str = "test_rs_console_forger pid=1 code=0 cpu=0ms";

/// A program spawned from a file named `exit`, writing the kernel's verdict
/// record for a job after the kernel's own: every judge of the kernel's records
/// in `/log` reads the job's real exit, and the forged line is there to be read.
pub fn console_record_cannot_forge_the_kernel(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let image_path = image_at("console-forger.img");
    let image = qemu::build_boot_image(test_config, c_bins, rust_bins, &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = log_extent(&image, &image_path)?;

    let mut guest = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            boot_image: Some(qemu::Staged::Written(image_path.clone())),
            ..Default::default()
        },
    );
    let ran = guest.run_test(FORGER, Duration::from_secs(60));
    if ran.exit_code != Some(FORGER_CODE as i32) {
        return Err(format!("{FORGER} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    // The copy says its line half a second after the job ended; the kernel's
    // record of the copy's own exit comes after that line.
    let copy_exited = format!("{}exit pid=", toyos_build::bootlog::EXIT);
    let said = guest.drain_until(Duration::from_secs(10), |l| l.contains(&copy_exited));
    if !said.contains(&copy_exited) {
        return Err(format!("the copy named exit never ended\n{said}"));
    }
    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU: {e}"))?;
    guest.flush_stdin();
    let _ = guest.drain_serial(Duration::from_secs(20));
    drop(guest);
    let (_, bytes) = newest_log(&image_path, start, len)?;
    let log = String::from_utf8_lossy(&bytes).into_owned();

    // Non-vacuity: the forgery reached the file, whatever form it took there.
    let forged: Vec<&str> =
        log.lines().filter(|l| toyos_build::bootlog::message(l).is_some_and(|m| m.ends_with(FORGED))).collect();
    if forged.is_empty() {
        return Err(format!("/log carries no line ending {FORGED:?}: nothing was forged\n{log}"));
    }
    let kernel = toyos_build::bootlog::kernel_records(&log);
    for (judge, text) in [("the whole log", log.as_str()), ("its kernel records", kernel.as_str())] {
        match toyos_build::metaldevices::exit_of(text, FORGER) {
            Some(exit) if exit.code == FORGER_CODE => {}
            other => {
                return Err(format!(
                    "the exit judge read {FORGER}'s verdict out of {judge} as {other:?}; it exited \
                     {FORGER_CODE}, and a program named `exit` wrote {forged:?}"
                ))
            }
        }
    }
    if let Some(line) = kernel.lines().find(|l| l.ends_with(FORGED)) {
        return Err(format!("a program's line is among the kernel's records: {line:?}"));
    }
    eprintln!("  [spoken] the forgery is in /log as {forged:?}, and every judge read exit {FORGER_CODE}");
    Ok(())
}
