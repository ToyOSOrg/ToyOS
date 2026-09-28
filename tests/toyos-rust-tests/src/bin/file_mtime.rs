//! A file's mtime is the wall clock at its write, in nanoseconds since the Unix
//! epoch (`toyos_abi::syscall::Stat::mtime`).
//!
//! With no arguments, on the shared boot, it judges `/tmp`: the stamp lies
//! between two `SYS_CLOCK_EPOCH` readings taken around the write, and a second
//! write's stamp is later than the first's, which a clock of whole seconds
//! cannot say. `write <path>` makes the first judgement on `path` and `read
//! <path>` only prints what it finds there; `file_mtime_survives_a_reboot`
//! (`tests/common/wallclock.rs`) drives the two across a reboot and holds the
//! printed stamp against the instant the host staged in the RTC.

use std::fs;
use std::io::Write;
use std::time::UNIX_EPOCH;

const NANOS_PER_SEC: u64 = 1_000_000_000;

fn mtime(path: &str) -> u64 {
    let modified = fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {path}: {e}"))
        .modified()
        .unwrap_or_else(|e| panic!("{path} has no mtime: {e}"));
    let since = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| panic!("{path}'s mtime is before the epoch"));
    u64::try_from(since.as_nanos()).expect("an mtime past 2554")
}

fn epoch() -> u64 {
    toyos::system::clock_epoch().expect("SYS_CLOCK_EPOCH: this machine will not say what time it is")
}

/// Writes `path` and returns its stamp, judged against the wall clock around the write.
fn write_judged(path: &str, bytes: &[u8]) -> u64 {
    let before = epoch();
    {
        let mut f = fs::File::create(path).unwrap_or_else(|e| panic!("create {path}: {e}"));
        f.write_all(bytes).unwrap_or_else(|e| panic!("write {path}: {e}"));
        f.sync_all().unwrap_or_else(|e| panic!("fsync {path}: {e}"));
    }
    let after = epoch();
    let stamp = mtime(path);
    // `SYS_CLOCK_EPOCH` is the whole seconds of the clock that stamps, so the
    // write's instant is at or past `before` and short of the second after `after`.
    assert!(
        before * NANOS_PER_SEC <= stamp && stamp < (after + 1) * NANOS_PER_SEC,
        "{path} is stamped {stamp} ns, and the wall clock read {before} s before the write and \
         {after} s after it",
    );
    stamp
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.as_slice() {
        [_] => {
            let first = write_judged("/tmp/file-mtime-first", b"first");
            let second = write_judged("/tmp/file-mtime-second", b"second");
            assert!(
                second > first,
                "a write after another is stamped {second} ns and the one before it {first} ns"
            );
            println!("file-mtime: /tmp stamps {first} then {second}");
        }
        [_, mode, path] if mode == "write" => {
            let stamp = write_judged(path, b"stamped at its write");
            println!("file-mtime: {path} mtime={stamp}");
        }
        [_, mode, path] if mode == "read" => {
            println!("file-mtime: {path} mtime={}", mtime(path));
        }
        _ => panic!("usage: file_mtime [write <path> | read <path>], got {args:?}"),
    }
}
