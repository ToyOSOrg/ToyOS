//! A declared row's image comes from the row's own path, resolved once — never
//! from a caller-writable path re-read after the row was named.
//!
//! The supervisor's launch path resolves `request.program` twice on the base: once
//! to name the `[programs]` row (and with it the row's capabilities), and again
//! to read the image. A `/tmp` symlink resolves to the declared row at the
//! first and to the caller's own binary at the second, so the caller's bytes
//! run holding the row's capability.
//!
//! This binary is both halves. The default role is the attacker: it points a
//! `/tmp` symlink at `/system/bin/toybox` (a declared row this config gives the
//! `roster` capability a plain job never holds), launches it through the
//! launcher while a second thread re-points the symlink at this binary, and
//! waits each launched child. The `evil` role is what the re-point substitutes:
//! it exits [`EXPLOIT`] when it holds the row's `SysCap` — proof the caller's
//! bytes ran under the declared row — and `0` otherwise.
//!
//! On the fix the supervisor reads the image from the row's `/system` path (an
//! immutable, kernel-served mount) into an object, so the kernel never re-opens
//! the caller's symlink and the `evil` bytes never run under the row. The race
//! then never resolves to [`EXPLOIT`] and the attacker exits `0`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use std::os::toyos::fs::symlink;

use toyos::endow::{Endowments, SYSCAP_LABEL};

/// This binary's own path, the bytes the re-point substitutes for the row's.
const SELF_PATH: &str = "/system/bin/test_rs_launch_toctou";
/// The declared row the symlink resolves to. `/system` is read-only to every
/// program, so the row's bytes cannot be rewritten — only which file the
/// caller's symlink names can.
const DECLARED: &str = "/system/bin/toybox";
/// The caller-writable symlink raced between the two paths.
const LINK: &str = "/tmp/x";

/// The `evil` role's exit code when it ran holding the declared row's `SysCap`.
/// Distinct from every code toybox or a plain spawn produces.
const EXPLOIT: i32 = 123;

/// How long the attacker keeps racing before it concludes the exploit never
/// landed. Well under the runner's one-job budget; the window the base leaves
/// open is the whole of the supervisor's `start`, so a win comes in far fewer
/// attempts than this bounds.
const RACE_BUDGET: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: u32 = 600;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("evil") => evil(),
        _ => attacker(),
    }
}

/// The caller's bytes. Reached only when the supervisor read this binary's image
/// for a launch it resolved to the `toybox` row.
fn evil() -> ! {
    // A launched instance of the `toybox` row is endowed that row's `SysCap`;
    // a plain child the attacker spawns directly is not. So holding one here is
    // proof these bytes ran under the row the caller never named for them.
    let under_row = Endowments::get().holds(SYSCAP_LABEL);
    std::process::exit(if under_row { EXPLOIT } else { 0 });
}

fn attacker() {
    // Start the symlink on the declared row, so the first resolution names it.
    let _ = std::fs::remove_file(LINK);
    symlink(DECLARED, LINK).expect("create /tmp/x -> the declared row");

    // Re-point forever between the row's path and this binary's. tmpfs displaces
    // the existing entry, so each call is one atomic swap of what `/tmp/x` names.
    std::thread::spawn(|| loop {
        let _ = symlink(SELF_PATH, LINK);
        let _ = symlink(DECLARED, LINK);
    });

    let deadline = Instant::now() + RACE_BUDGET;
    let mut exploited = false;
    let mut attempts = 0u32;
    while attempts < MAX_ATTEMPTS && Instant::now() < deadline {
        attempts += 1;
        // A served working directory, judged by a file server mid-launch, is
        // one of the places the base's second resolution waits — widening the
        // window the re-point has to land in.
        let spawned = Command::new(LINK)
            .arg("evil")
            .current_dir("/home")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { continue };
        if child.wait().ok().and_then(|s| s.code()) == Some(EXPLOIT) {
            exploited = true;
            break;
        }
    }

    assert!(
        !exploited,
        "launch_toctou: the caller's bytes ran holding the `toybox` row's capability after \
         {attempts} launch(es): the supervisor read a declared row's image from the caller's \
         path, re-read after the row was named"
    );
    println!("launch_toctou: PASS ({attempts} launches, none ran the caller's bytes under the row)");
}
