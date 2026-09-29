//! Two callers of the stop, and the one that is refused.
//!
//! init makes the first call, asked through its `power` port.
//! `quiesce-last-park` holds that call once it has
//! claimed the stop and before it stops anything, until a thread named
//! [`LAST_THREAD`] parks: the window in which every other thread still runs.
//! This process reads the kernel's log until the kernel says it waits there,
//! makes the second call itself, and only on being refused by name starts the
//! thread the stop waits for — so a stop that completes is that refusal
//! having reached Ring 3 as a word.
//!
//! Nothing here asserts: `common::power::quiesce_refuses_a_second_shutdown` is
//! the judge, and its doc is the scenario.

use std::time::Duration;

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

/// Records per read; above the shard count, which the call refuses.
const BATCH: usize = 4 * MAX_LOG_SHARDS as usize;

/// The poll's one token.
const LOG_TOKEN: u64 = 1;

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_twice: this program was endowed no system capability");
        std::process::exit(1);
    };
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
        // No deadline: a first call that never waits is a hang the harness
        // ceiling reds.
        poller.wait(1, u64::MAX, |_| {});
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
        .spawn(asleep_until_stopped)
        .expect("spawn the thread the stop waits for");
    // No deadline: a machine that never stops this process is a hang the
    // harness ceiling reds.
    asleep_until_stopped()
}

/// Asleep until the machine stops, which is the only thing that ends it. A
/// sleep and not a park because `nanosleep` is the syscall `quiesce-last-park`
/// holds the named thread in; its span is never reached.
fn asleep_until_stopped() -> ! {
    loop {
        std::thread::sleep(Duration::MAX);
    }
}
