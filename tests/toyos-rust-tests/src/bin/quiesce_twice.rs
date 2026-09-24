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
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::log::{LogTail, Record, MAX_LOG_SHARDS};
use toyos::poller::{Poller, READABLE};
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

/// The poll's one token.
const LOG_TOKEN: u64 = 1;

/// How long the first caller's drain gets to be refused before this boot
/// says it never was. Inside the harness's own wait for the machine to stop.
const REFUSAL_WITHIN: Duration = Duration::from_secs(10);

/// How long the machine gets to stop this thread after its refusal before it
/// says it was never stopped.
const STOPPED_WITHIN: Duration = Duration::from_secs(20);

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

    // Parked on the log's readiness between reads. The readiness is an edge,
    // so each watch is armed before the read it guards, and one is
    // outstanding at a time.
    let poller = Poller::new(1);
    let give_up = Instant::now() + REFUSAL_WITHIN;
    poller.watch(cap, READABLE, LOG_TOKEN);
    poller.wait(0, 0, |_| {});
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
        if !batch.is_empty() {
            continue;
        }
        let Some(left) = give_up.checked_duration_since(Instant::now()) else {
            eprintln!(
                "quiesce_twice: the first caller's drain was not refused in {REFUSAL_WITHIN:?}"
            );
            std::process::exit(1);
        };
        poller.wait(1, left.as_nanos() as u64, |_| {});
        poller.watch(cap, READABLE, LOG_TOKEN);
        poller.wait(0, 0, |_| {});
    }
    println!("quiesce_twice: the first caller's drain is being refused; asking again");
    // The other power syscall, so the one boot judges the claim on both: moved
    // into either one alone, this call or the first caller's is let in.
    let refused = cap.shutdown();
    println!("quiesce_twice: the second caller was refused ({refused:?})");
    // Carved out of the first stage, so this thread runs until the second
    // stops it where it sleeps; nothing tells a thread it has been stopped,
    // so the bound is the only word it can have.
    std::thread::sleep(STOPPED_WITHIN);
    eprintln!("quiesce_twice: the machine did not stop this thread in {STOPPED_WITHIN:?}");
    std::process::exit(1);
}
