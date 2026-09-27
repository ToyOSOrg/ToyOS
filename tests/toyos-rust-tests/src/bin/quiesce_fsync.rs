//! A flush parked over split FATs, and the machine's stop asked for above it.
//!
//! `quiesce-fsync-refuse` refuses the active FAT's write of every `SYS_FSYNC`
//! attempt on [`STAGED`] until its ladder closes, and the park between two
//! attempts is where a stop finds the caller. One thread here makes that
//! fsync; this one reads the kernel's log until the attempt that parks is
//! refused, and asks init for the shutdown — so the stop meets the thread
//! parked with the two FATs disagreeing.
//!
//! Nothing here asserts: `common::volumes::quiesce_leaves_the_volume_whole`
//! reads the volume and the kernel's lines.

use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::log::{LogTail, Record, MAX_LOG_SHARDS};
use toyos::poller::{Poller, READABLE};
use toyos::power::Stop;
use toyos::syscap::SysCap;

/// The file the actuator refuses the flush of. Mirrored from the kernel's
/// `fat32_adapter::FSYNC_STAGED`.
const STAGED: &str = "/log/quiesce-fsync.bin";

/// The kernel's word for the second refused attempt: the first only yields,
/// and the second is the one whose caller parks.
const PARKS: &str = "quiesce-fsync-refuse: refusing a SYS_FSYNC flush's active-FAT write";
const SECOND: &str = ", 2 of ";

/// Enough clusters that the flush allocates, and so writes the FAT.
const CHUNK: usize = 8192;
const CHUNKS: usize = 8;

/// Records per read; above the shard count, which the call refuses.
const BATCH: usize = 4 * MAX_LOG_SHARDS as usize;
const LOG_TOKEN: u64 = 1;

/// How long the fsync gets to be refused the second time.
const PARKS_WITHIN: Duration = Duration::from_secs(5);

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_fsync: this program was endowed no system capability");
        std::process::exit(1);
    };
    std::thread::spawn(|| {
        let mut f = File::create(STAGED).unwrap_or_else(|e| {
            eprintln!("quiesce_fsync: could not create {STAGED}: {e}");
            std::process::exit(1);
        });
        for _ in 0..CHUNKS {
            f.write_all(&[b'f'; CHUNK]).unwrap_or_else(|e| {
                eprintln!("quiesce_fsync: could not write {STAGED}: {e}");
                std::process::exit(1);
            });
        }
        // Comes back once the ladder closes, which the stop waits for.
        if let Err(e) = f.sync_all() {
            eprintln!("quiesce_fsync: the fsync failed: {e}");
            std::process::exit(1);
        }
    });

    let mut tail = LogTail::new();
    let mut buf = [Record::EMPTY; BATCH];
    let poller = Poller::new(1);
    let give_up = Instant::now() + PARKS_WITHIN;
    poller.watch(&cap, READABLE, LOG_TOKEN);
    poller.wait(0, 0, |_| {});
    loop {
        let batch = tail.read(&cap, &mut buf).unwrap_or_else(|e| {
            eprintln!("quiesce_fsync: the log would not read ({e:?})");
            std::process::exit(1);
        });
        if batch.iter().any(|r| r.message().contains(PARKS) && r.message().contains(SECOND)) {
            break;
        }
        if !batch.is_empty() {
            continue;
        }
        let Some(left) = give_up.checked_duration_since(Instant::now()) else {
            eprintln!("quiesce_fsync: the fsync of {STAGED} was not refused twice in {PARKS_WITHIN:?}");
            std::process::exit(1);
        };
        poller.wait(1, left.as_nanos() as u64, |_| {});
        poller.watch(&cap, READABLE, LOG_TOKEN);
        poller.wait(0, 0, |_| {});
    }
    let refused = toyos::power::stop(Stop::Shutdown);
    eprintln!("quiesce_fsync: init did not stop the machine ({refused:?})");
    std::process::exit(1);
}
