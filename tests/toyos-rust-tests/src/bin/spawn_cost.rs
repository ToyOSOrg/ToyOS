//! What starting a program costs: `/system/bin/shell -c ''` spawned and waited
//! for. It returns as soon as it has started, so what the clock reads is the
//! kernel's spawn, the program's own start, its relocation among it, and its
//! exit.
//!
//! **No threshold, and the host reads the numbers instead**, as
//! `syscall_cost`'s: a duration measured under TCG is no duration, and one
//! measured on metal is for a same-session A/B against another build of this
//! tree. The minimum and the median over repetitions, after a first spawn that
//! reads the file's pages off the device.

use std::process::Command;

use toyos_abi::clock::nanos_since_boot as clock_nanos;

const SHELL: &str = "/system/bin/shell";
const REPS: usize = 50;

fn spawn_ns() -> u64 {
    let start = clock_nanos();
    let status = Command::new(SHELL).args(["-c", ""]).status().unwrap_or_else(|e| panic!("spawn {SHELL}: {e}"));
    let end = clock_nanos();
    assert!(status.success(), "{SHELL} -c '' exited {status:?}");
    end - start
}

fn main() {
    spawn_ns();
    let mut ns: Vec<u64> = (0..REPS).map(|_| spawn_ns()).collect();
    ns.sort_unstable();
    println!("spawn_cost: {SHELL} min {} us median {} us over {REPS}", ns[0] / 1000, ns[REPS / 2] / 1000);
}
