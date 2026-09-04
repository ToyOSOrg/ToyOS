//! `mkdir` must refuse past the kernel's created-directory cap, never grow a
//! kernel `HashSet` without bound.
//!
//! `Vfs::create_dir` inserts a userland-chosen path key under the one VFS lock
//! and had no ceiling, so a `mkdir` loop grew it until the kernel heap gave out.
//! Its own boot, because it fills the cap and leaves it there.

use std::fs;
use std::process::Command;

/// `vfs::MAX_CREATED_DIRS`; this tracks the kernel's number, so a change to one is a change to both.
const MAX_CREATED_DIRS: usize = 16_384;

/// A margin past the cap: boot fills a few entries first, so the refusal lands under this many tries.
const TRIES: usize = MAX_CREATED_DIRS + 512;

fn main() {
    mkdir_is_refused_past_the_cap();
    a_repeat_of_a_held_directory_still_succeeds();
    system_alive();
    println!("all mkdir cap tests passed");
}

/// Distinct directories up to the cap, then an error rather than a panic or an unbounded grow.
fn mkdir_is_refused_past_the_cap() {
    fs::create_dir("/tmp/h1").expect("create /tmp/h1");
    for i in 0..TRIES {
        if let Err(e) = fs::create_dir(format!("/tmp/h1/d{i}")) {
            assert!(i > 0, "the very first mkdir was refused: {e}");
            println!("  PASS: mkdir refused at {i} of {TRIES} ({e})");
            return;
        }
    }
    panic!("mkdir never refused in {TRIES} tries: created_dirs is unbounded");
}

/// At the cap, a directory already held is let through: the refusal is on new keys, not the count alone.
fn a_repeat_of_a_held_directory_still_succeeds() {
    fs::create_dir("/tmp/h1/d0").expect("re-creating a held directory at the cap must be let through");
    println!("  PASS: a directory already held is let through at the cap");
}

/// A spawn after the refusal — it reads the binary through the same VFS lock a panic would have stranded.
fn system_alive() {
    let output = Command::new("/system/bin/echo")
        .arg("still alive")
        .output()
        .expect("failed to run echo after the refusal");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "still alive");
    println!("  PASS: the VFS still serves after the refusal");
}
