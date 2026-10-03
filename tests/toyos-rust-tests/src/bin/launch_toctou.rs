//! A launched row runs the row's own program, never the bytes a caller's path
//! names when it is opened again.
//!
//! The launcher names a `[programs]` row from the caller's path, following one
//! link, and the row's capabilities go to whatever it then starts. This binary
//! points a `/tmp` link at `/system/bin/toybox` — a row this boot gives the
//! `roster` capability — launches it under the name `ps` while a second thread
//! re-points the link between toybox and this binary, and judges every child by
//! its exit:
//!
//! - toybox's `ps` exits 0 only holding a `SysCap` with `ROSTER`, which only the
//!   row gives: the row's own program under the row;
//! - `ps` without one exits 1: the link named this binary when the launcher
//!   asked, so the caller spawned it directly, and it named toybox again when the
//!   kernel opened it;
//! - this binary as [`ROLE`] exits [`PLAIN`] without a `SysCap` and [`EXPLOIT`]
//!   with one — the caller's bytes under the row's claims, the defect.
//!
//! Every launch must start and be waited, and both of the first two must happen:
//! one shows a launch of the row runs, the other that the link really moved
//! while launches were made.

use std::os::toyos::fs::symlink;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use toyos::endow::{Endowments, SYSCAP_LABEL};

/// This binary, the bytes a re-point substitutes for the row's.
const SELF_PATH: &str = "/system/bin/test_rs_launch_toctou";
/// The declared row the link names. `/system` is read-only to every program,
/// so only which file the link names can move, never the row's bytes.
const DECLARED: &str = "/system/bin/toybox";
/// The caller-writable link, named for the applet toybox runs as.
const LINK: &str = "/tmp/ps";
/// The argument that makes a child of this binary [`role`] rather than the
/// attacker.
const ROLE: &str = "role";

/// This binary as a child, holding a `SysCap`: only a launch of the row gives one.
const EXPLOIT: i32 = 123;
/// This binary as a child, holding none.
const PLAIN: i32 = 77;
/// Toybox's `ps`, having read the roster.
const PS_UNDER_ROW: i32 = 0;
/// Toybox's `ps`, endowed no `SysCap`.
const PS_DIRECT: i32 = 1;

const ATTEMPTS: u32 = 100;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(ROLE) => role(),
        _ => attacker(),
    }
}

/// The caller's bytes, run by a launch.
fn role() -> ! {
    let under_row = Endowments::get().holds(SYSCAP_LABEL);
    std::process::exit(if under_row { EXPLOIT } else { PLAIN });
}

fn attacker() {
    let _ = std::fs::remove_file(LINK);
    symlink(DECLARED, LINK).expect("create the link at the declared row");

    let stop = Arc::new(AtomicBool::new(false));
    let flipper = std::thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            // tmpfs displaces the existing entry, so each call is one swap of
            // what the link names.
            while !stop.load(Ordering::Relaxed) {
                symlink(SELF_PATH, LINK).expect("re-point the link at this binary");
                symlink(DECLARED, LINK).expect("re-point the link at the declared row");
            }
        }
    });

    let (mut under_row, mut direct) = (0u32, 0u32);
    for attempt in 1..=ATTEMPTS {
        // Captured rather than null: a direct spawn holds no slot it was not
        // given, and `ps` says why it refused on stderr.
        let output = Command::new(LINK)
            .arg(ROLE)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("launch {attempt} of {LINK} was refused or not waited: {e}"));
        match output.status.code() {
            Some(PS_UNDER_ROW) => under_row += 1,
            Some(PS_DIRECT) | Some(PLAIN) => direct += 1,
            Some(EXPLOIT) => panic!(
                "launch_toctou: launch {attempt} ran the caller's bytes holding the `toybox` \
                 row's capability: the program a launched row runs was opened from the \
                 caller's path"
            ),
            other => panic!("launch {attempt} exited {other:?}, which neither program answers"),
        }
    }

    stop.store(true, Ordering::Relaxed);
    flipper.join().expect("the re-point thread failed");
    assert!(under_row > 0, "none of {ATTEMPTS} launches ran the `toybox` row's program under its row");
    assert!(direct > 0, "the link never named this binary when the launcher asked in {ATTEMPTS} launches");
    println!("launch_toctou: PASS ({under_row} of {ATTEMPTS} launches ran the row's program under it, none the caller's)");
}
