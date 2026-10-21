//! A launch fills each stdio slot its caller sent nothing for with a null,
//! never the supervisor's own: `launch_unsent_stdio`'s launched `cat` reads
//! end of file in the guest, and here its launched `echo` given no stdout
//! says nothing anywhere on the console, the supervisor's lines included.
//!
//! On QEMU x86-64 (`tests/images/launchcase.toml`): what it claims is
//! completion and contents, and no hardware is in it. A boot of its own,
//! because its launcher turns every spawn of a declared program into a launch,
//! which `libc_children`'s programs must not be.

use std::time::Duration;

use super::{compile, qemu};
use qemu::{BootOptions, QemuInstance};

const JOB: &str = "launch_unsent_stdio";
/// The guest's two words: the one its echo given no stdout is asked to say,
/// and the one its echo given the job's stdout says.
const UNSENT: &str = "nothing-sent-said-this";
const SENT: &str = "slot-sent-said-this";
const BOUND: Duration = Duration::from_secs(60);

pub fn a_launch_gives_each_unsent_stdio_slot_a_null() -> Result<(), String> {
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let case = compile::repo_root().join("tests/images/launchcase.toml");
    let mut qemu = QemuInstance::boot_with_options(&case, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    let result = qemu.run_test(&format!("test_rs_{JOB}"), BOUND);
    if result.error.is_some() || result.exit_code != Some(0) {
        return Err(format!("{JOB} ended {:?} {:?}:\n{}", result.exit_code, result.error, result.serial));
    }
    // Two rings, each drained on its own: the sent word, launched after the
    // unsent one ended, is what says the console has caught up.
    let mut console = format!("{}\n{}", qemu.boot_log(), result.serial);
    if !console.contains(SENT) {
        console.push_str(&qemu.drain_until(BOUND, |line| line.contains(SENT)));
    }
    if !console.contains(SENT) {
        return Err(format!("the echo given the job's stdout never said {SENT}:\n{console}"));
    }
    if let Some(line) = console.lines().find(|line| line.contains(UNSENT)) {
        return Err(format!("the echo given no stdout was heard: {line}"));
    }
    eprintln!("  [launch] an echo launched with no stdout said nothing; one given the job's said {SENT}");
    Ok(())
}
