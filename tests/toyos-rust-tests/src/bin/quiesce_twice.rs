//! Two callers of the stop, and the one that is refused.
//!
//! init makes the first call, asked through its `power` port, after a closed
//! file's flush is left owed. `quiesce-last-park` holds that call once it has
//! claimed the stop and before it stops anything, until a thread named
//! [`LAST_THREAD`] parks: the window in which every other thread still runs.
//! This process reads the kernel's log until the kernel says it waits there,
//! makes the second call itself, and only on being refused by name starts the
//! thread the stop waits for — so a stop that completes is that refusal
//! having reached Ring 3 as a word.
//!
//! Nothing here asserts: `common::power::quiesce_refuses_a_second_shutdown` is
//! the judge, and its doc is the scenario.

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::log::{LogTail, Record, MAX_LOG_SHARDS};
use toyos::poller::{Poller, READABLE};
use toyos::power::Stop;
use toyos::syscap::SysCap;
use toyos_abi::syscall::SyscallError;
use toyos_quiesce::LAST_THREAD;

/// What the kernel says once the first call has claimed the stop and waits
/// for the held thread.
const WAITS: &str = "quiesce-last-park: the stop waits for";

/// Bytes the owed file carries: enough clusters that its flush writes the
/// FAT, whose mirror half `quiesce-drain-refuse` refuses the stop's own drain.
const CHUNK: usize = 8192;
const CHUNKS: usize = 8;

/// Records per read; above the shard count, which the call refuses.
const BATCH: usize = 4 * MAX_LOG_SHARDS as usize;

/// The poll's one token.
const LOG_TOKEN: u64 = 1;

/// How long the first call gets to reach its wait before this boot says it
/// never did: inside the kernel's own bound on that wait, so this line is the
/// one that says why.
const WAITS_WITHIN: Duration = Duration::from_secs(5);

/// How long the machine gets to stop this process once the held thread runs.
const STOPPED_WITHIN: Duration = Duration::from_secs(20);

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_twice: this program was endowed no system capability");
        std::process::exit(1);
    };
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

    // The first call, from init. Comes back only refused.
    std::thread::spawn(|| {
        let refused = toyos::power::stop(Stop::Reboot);
        eprintln!("quiesce_twice: init did not stop the machine ({refused:?})");
        std::process::exit(1);
    });

    // Parked on the log's readiness between reads. The readiness is an edge,
    // so each watch is armed before the read it guards, and one is
    // outstanding at a time.
    let mut tail = LogTail::new();
    let mut buf = [Record::EMPTY; BATCH];
    let poller = Poller::new(1);
    let give_up = Instant::now() + WAITS_WITHIN;
    poller.watch(&cap, READABLE, LOG_TOKEN);
    poller.wait(0, 0, |_| {});
    loop {
        let batch = tail.read(&cap, &mut buf).unwrap_or_else(|e| {
            eprintln!("quiesce_twice: the log would not read ({e:?})");
            std::process::exit(1);
        });
        if batch.iter().any(|record| record.message().contains(WAITS)) {
            break;
        }
        if !batch.is_empty() {
            continue;
        }
        let Some(left) = give_up.checked_duration_since(Instant::now()) else {
            eprintln!("quiesce_twice: the first call never waited for {LAST_THREAD}");
            std::process::exit(1);
        };
        poller.wait(1, left.as_nanos() as u64, |_| {});
        poller.watch(&cap, READABLE, LOG_TOKEN);
        poller.wait(0, 0, |_| {});
    }

    // The other power syscall, so the one boot judges the claim on both.
    let refused = cap.shutdown();
    if refused != SyscallError::AlreadyExists {
        eprintln!("quiesce_twice: the second call was answered {refused:?}, not AlreadyExists");
        std::process::exit(1);
    }
    std::thread::Builder::new()
        .name(LAST_THREAD.into())
        .spawn(|| std::thread::sleep(STOPPED_WITHIN))
        .expect("spawn the thread the stop waits for");
    std::thread::sleep(STOPPED_WITHIN);
    eprintln!("quiesce_twice: the machine did not stop this process in {STOPPED_WITHIN:?}");
    std::process::exit(1);
}
