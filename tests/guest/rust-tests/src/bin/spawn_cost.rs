//! What starting a program costs: `/system/bin/toybox echo` spawned and waited
//! for, its line read off a pipe, since a null stdout is a slot the child does
//! not hold (`issues/there-is-no-dev-null.md`). It returns as soon as it has started, so what the
//! clock reads is the kernel's spawn, the program's own start, its relocation
//! among it, and its exit. `toybox` because it is on the ROOT of the boot this
//! runs on, `tests/images/testcases.toml`, and carries the most `RELATIVE` entries of what
//! that ROOT does.
//!
//! **No threshold, and the host reads the numbers instead**, as
//! `syscall_cost`'s: a duration measured under TCG is no duration, and one
//! measured on metal is for a same-session A/B against another build of this
//! tree. The minimum and the median over repetitions, after a first spawn that
//! reads the file's pages off the device.

use std::process::{Command, Stdio};

use toyos_abi::clock::nanos_since_boot as clock_nanos;

const PROGRAM: &str = "/system/bin/toybox";
const REPS: usize = 50;

fn spawn_ns() -> u64 {
    let start = clock_nanos();
    let out = Command::new(PROGRAM)
        .arg("echo")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("spawn {PROGRAM}: {e}"));
    let end = clock_nanos();
    assert!(out.status.success(), "{PROGRAM} echo ended {}", out.status);
    end - start
}

fn main() {
    spawn_ns();
    let mut ns: Vec<u64> = (0..REPS).map(|_| spawn_ns()).collect();
    ns.sort_unstable();
    println!("spawn_cost: {PROGRAM} min {} us median {} us over {REPS}", ns[0] / 1000, ns[REPS / 2] / 1000);
}
