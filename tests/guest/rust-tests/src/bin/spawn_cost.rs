//! What starting a program costs: `/system/bin/toybox echo` spawned with null
//! stdio and waited for. It returns as soon as it has started, so what the
//! clock reads is the kernel's spawn, the program's own start, its relocation
//! among it, and its exit. `toybox` because it is on the ROOT of the boot this
//! runs on, `tests/images/testcases.toml`, and carries the most `RELATIVE` entries of what
//! that ROOT does.
//!
//! **And what it costs the machine**: the CPU every CPU was busy for over the
//! repetitions, a spawn's share, which counts what the spawn costs the
//! processes it reaches off its own path, the log among them. Work a spawn
//! leaves for later than the last repetition is not in it.
//!
//! **No threshold, and the host reads the numbers instead**, as
//! `syscall_cost`'s: a duration measured under TCG is no duration, and one
//! measured on metal is for a same-session A/B against another build of this
//! tree. The minimum and the median over repetitions, after a first spawn that
//! reads the file's pages off the device.

use std::process::{Command, Stdio};

use toyos_abi::clock::nanos_since_boot as clock_nanos;
use toyos_abi::syscall::{SysinfoHeader, SYSINFO_HEADER_SIZE};

const PROGRAM: &str = "/system/bin/toybox";
const REPS: usize = 50;

fn spawn_ns() -> u64 {
    let start = clock_nanos();
    let status = Command::new(PROGRAM)
        .arg("echo")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("spawn {PROGRAM}: {e}"));
    let end = clock_nanos();
    assert!(status.success(), "{PROGRAM} echo ended {status}");
    end - start
}

/// The CPU time every CPU has been busy for since the boot.
fn busy_ns() -> u64 {
    let mut header = [0u8; SYSINFO_HEADER_SIZE];
    let n = toyos::system::sysinfo(&mut header);
    assert!(n >= SYSINFO_HEADER_SIZE, "sysinfo returned {n} bytes");
    SysinfoHeader::decode(&header).total_cpu_ns
}

fn main() {
    spawn_ns();
    let busy = busy_ns();
    let mut ns: Vec<u64> = (0..REPS).map(|_| spawn_ns()).collect();
    let machine = (busy_ns() - busy) / REPS as u64;
    ns.sort_unstable();
    println!(
        "spawn_cost: {PROGRAM} min {} us median {} us over {REPS}, the machine busy {} us a spawn",
        ns[0] / 1000,
        ns[REPS / 2] / 1000,
        machine / 1000
    );
}
