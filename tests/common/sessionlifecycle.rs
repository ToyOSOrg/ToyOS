//! Login sessions, stage 5 of
//! `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`,
//! in one boot of `tests/images/sessioncase.toml` on QEMU x86-64:
//!
//! - **the desktop's**, opened for the compositor as it starts: Ctrl+N's
//!   terminal detaches `hangupholder`, typed at its shell, and Ctrl+Alt+Q
//!   signs the session out, waiting for the holder; Escape cancels and keeps
//!   it, and Enter ends it, the compositor opening another;
//! - **a job's**, opened with test-runner's `login` connector:
//!   `session_lifecycle` holds the owner to the same rules from inside, with
//!   `logout` listing it, and opens ten in a row unbudgeted; last, it asks
//!   the machine's reboot, which waits for the holder until a cancel keeps
//!   both and a terminate forces it, and the boot ends in the reset.
//!
//! What it claims is order, completion and counts, which QEMU establishes.

use std::io::Write;
use std::time::Duration;

use toyos_build::bootlog::REBOOTING;

use super::qemu::{self, await_marker, await_marker_new, BootOptions, QemuInstance, QmpInput};
use super::{compile, power, ps2server};

const JOB: &str = "session_lifecycle";

/// A hang ceiling: its job's own waits are bounded below it.
const BOUND: Duration = Duration::from_secs(300);

pub fn session_lifecycle() -> Result<(), String> {
    let case = compile::repo_root().join("tests/images/sessioncase.toml");
    let rust_tests = compile::repo_root().join("tests/guest/rust-tests");
    let job = qemu::build_toyos_bin(qemu::SUITE_ARCH, &rust_tests, JOB);
    let options = BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(&case, &[], &[(JOB.to_string(), job)], options);
    let mut console = format!("{}\n", qemu.boot_log());
    desktop(&mut qemu, &mut console).map_err(|e| {
        let said: Vec<&str> = console.lines().filter(|l| l.contains("compositor") || l.contains("session") || l.contains("hangup")).collect();
        format!("{e}\n--- what the desktop said ---\n{}", said.join("\n"))
    })?;

    // The job ends in the reboot it forces, so its end is the machine's.
    let mut reset = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(BOUND));
    let from = console.len();
    writeln!(qemu.stdin_mut(), "run test_rs_{JOB}").map_err(|e| e.to_string())?;
    qemu.flush_stdin();
    let whole = format!("{JOB}: ok, and the reboot is forced");
    await_marker_new(&mut qemu, &mut console, &whole, from, "the job to reach the forced reboot")?;
    power::ended(&mut qemu, &mut reset, &mut console, REBOOTING, "guest-reset")?;
    let said = &console[from..];
    for line in said.lines().filter(|l| l.contains(&format!("{JOB}: "))) {
        eprintln!("  [{JOB}] {}", line.trim());
    }
    // The kill is the terminate's and no clock's: after the job forced it.
    let forced = said.find(&whole).expect("awaited above");
    let killed = said.find("power: the stop ends what stays").ok_or("the supervisor never said what the forced reboot ended")?;
    if killed < forced {
        return Err(format!("the reboot ended what stays before the job forced it:\n{said}"));
    }
    Ok(())
}

/// The desktop session, driven through its keys.
fn desktop(qemu: &mut QemuInstance, console: &mut String) -> Result<(), String> {
    await_marker(qemu, console, "opened for compositor", "the desktop session to open")?;
    await_marker(qemu, console, "compositor: ready", "the compositor to take its panel")?;
    await_marker(qemu, console, ps2server::ARMED_BOTH, "ps2server to arm the i8042")?;
    let press = |qemu: &QemuInstance, chord: &[&str]| {
        let mut input = QmpInput::open(qemu.qmp_socket());
        input.keys(&chord.iter().map(|k| (*k, true)).collect::<Vec<_>>());
        input.keys(&chord.iter().rev().map(|k| (*k, false)).collect::<Vec<_>>());
    };

    let from = console.len();
    press(qemu, &["ctrl", "n"]);
    await_marker_new(qemu, console, "spawn: /system/bin/shell", from, "Ctrl+N's terminal to start its shell")?;
    await_marker_new(qemu, console, "terminal: ready", from, "the terminal to take keys")?;
    let from = console.len();
    for key in qcodes("detach /system/bin/hangupholder") {
        press(qemu, &[key]);
    }
    press(qemu, &["ret"]);
    await_marker_new(qemu, console, "spawn: /system/bin/hangupholder", from, "the shell to detach the holder")?;

    let waiting = "compositor: signing out, waiting for: hangupholder";
    let from = console.len();
    press(qemu, &["ctrl", "alt", "q"]);
    await_marker_new(qemu, console, waiting, from, "the sign-out to wait for the holder alone")?;
    let from = console.len();
    press(qemu, &["esc"]);
    await_marker_new(qemu, console, "compositor: the sign-out is cancelled", from, "Escape to cancel the sign-out")?;
    let from = console.len();
    press(qemu, &["ctrl", "alt", "q"]);
    await_marker_new(qemu, console, waiting, from, "a second sign-out to wait for the holder")?;
    let from = console.len();
    press(qemu, &["ret"]);
    await_marker_new(qemu, console, "compositor: signing out, ending what still runs", from, "Enter to force the sign-out")?;
    await_marker_new(qemu, console, "is signed out", from, "the forced session to end")?;
    let holder_end = console[from..].lines().find(|l| l.contains("exit: hangupholder pid=")).unwrap_or("");
    if !holder_end.contains(" killed ") {
        return Err(format!("the force did not kill the holder, which ended: {holder_end:?}"));
    }
    await_marker_new(qemu, console, "compositor: a desktop session is open again", from, "the desktop to open again")?;
    eprintln!("  [{JOB}] the desktop's sign-out waited for the holder, Escape kept it, and Enter ended it");
    Ok(())
}

/// QMP's key names for `text`: lowercase letters, `/` and spaces.
fn qcodes(text: &str) -> Vec<&'static str> {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v",
        "w", "x", "y", "z",
    ];
    text.chars()
        .map(|c| match c {
            'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
            '/' => "slash",
            ' ' => "spc",
            other => panic!("no key is typed for {other:?}"),
        })
        .collect()
}
