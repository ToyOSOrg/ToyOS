//! Two callers of the reset, from two processes, and the one that is refused.
//!
//! A child process leaves a closed file's flush owed and asks for the reset;
//! this process reads the kernel's log until that flush is being refused, and
//! asks again.
//!
//! Nothing here asserts: `common::power::quiesce_refuses_a_second_shutdown` is
//! the judge, and its doc is the scenario.

use std::fs::File;
use std::io::Write;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::log::{LogTail, Record, MAX_LOG_SHARDS};
use toyos::syscap::SysCap;

const SELF_PATH: &str = "/system/bin/test_rs_quiesce_twice";

/// The role the child is spawned with.
const FIRST: &str = "first";

/// What the kernel writes each time the actuator refuses the first caller's
/// drain: the first caller is inside its shutdown's retry ladder from the
/// first of these on.
const REFUSED: &str = "quiesce-drain-refuse: refusing the shutdown drain's";

/// Bytes the first caller leaves owed: enough clusters that the flush writes
/// the FAT, whose mirror half is what the actuator refuses.
const CHUNK: usize = 8192;
const CHUNKS: usize = 8;

/// Records per read; above the shard count, which the call refuses.
const BATCH: usize = 4 * MAX_LOG_SHARDS as usize;

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_twice: this program was endowed no system capability");
        std::process::exit(1);
    };
    if std::env::args().nth(1).as_deref() == Some(FIRST) {
        first_caller(&cap);
    }
    second_caller(&cap);
}

/// Leave a closed file's flush owed, then ask for the reset. Comes back only
/// refused: on the other path this thread is running the shutdown, parked in
/// the drain's ladder, until the machine is at its firmware.
fn first_caller(cap: &SysCap) -> ! {
    {
        let mut f = File::create("/log/quiesce-owed.bin").unwrap_or_else(|e| {
            eprintln!("quiesce_twice: could not create the owed file: {e}");
            std::process::exit(1);
        });
        for _ in 0..CHUNKS {
            f.write_all(&[b'o'; CHUNK]).unwrap_or_else(|e| {
                eprintln!("quiesce_twice: could not write the owed file: {e}");
                std::process::exit(1);
            });
        }
    }
    println!("quiesce_twice: the first caller asks for the reset");
    let refused = cap.reboot();
    eprintln!("quiesce_twice: the first reboot was refused ({refused:?})");
    std::process::exit(1);
}

/// Read the log, spawn the first caller, and ask for the reset the moment the
/// kernel says the first caller's drain is being refused.
fn second_caller(cap: &SysCap) -> ! {
    let mut tail = LogTail::new();
    let mut buf = [Record::EMPTY; BATCH];
    // Before the child exists: this read is what makes this process a holder
    // the stop's first stage leaves running, and a holder is recorded at its
    // first read, not at its endowment.
    if let Err(e) = tail.read(cap, &mut buf) {
        eprintln!("quiesce_twice: the log would not read ({e:?})");
        std::process::exit(1);
    }

    let dup = cap.duplicate().unwrap_or_else(|e| {
        eprintln!("quiesce_twice: the capability would not duplicate ({e:?})");
        std::process::exit(1);
    });
    let mut child = Command::new(SELF_PATH)
        .arg(FIRST)
        .stdin(Stdio::piped())
        .endow(SYSCAP_LABEL, dup.into_raw().0)
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("quiesce_twice: could not spawn the first caller: {e}");
            std::process::exit(1);
        });
    drop(child.stdin.take());

    loop {
        let batch = match tail.read(cap, &mut buf) {
            Ok(batch) => batch,
            Err(e) => {
                eprintln!("quiesce_twice: the log would not read ({e:?})");
                std::process::exit(1);
            }
        };
        if batch.iter().any(|record| record.message().contains(REFUSED)) {
            break;
        }
        std::thread::yield_now();
    }
    println!("quiesce_twice: the first caller's drain is being refused; asking again");
    // The other power syscall, so the one boot judges the claim on both: moved
    // into either one alone, this call or the first caller's is let in.
    let refused = cap.shutdown();
    println!("quiesce_twice: the second caller was refused ({refused:?})");
    // Carved out of the first stage, so this thread runs until the second;
    // it has nothing left to do but wait for that.
    loop {
        std::thread::yield_now();
    }
}
