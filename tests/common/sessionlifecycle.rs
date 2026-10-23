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
//!   `logout` listing it, and opens ten in a row unbudgeted.
//!
//! What it claims is order, completion and counts, which QEMU establishes.

use std::time::Duration;

use super::qemu::{self, await_marker, await_marker_new, BootOptions, QemuInstance, QmpInput};
use super::{compile, ps2server};

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

    let result = qemu.run_test(&format!("test_rs_{JOB}"), BOUND);
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for said in result.stdout.lines().filter(|l| l.contains(&format!("{JOB}: "))) {
        eprintln!("  [{JOB}] {}", said.trim());
    }
    match result.stdout.lines().any(|l| l.trim_end().ends_with(&format!("{JOB}: ok"))) {
        true => Ok(()),
        false => Err(format!("the job never said it was whole:\n{}", result.stdout)),
    }
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
