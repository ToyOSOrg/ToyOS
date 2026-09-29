//! A file's mtime is the wall clock at its write, in nanoseconds since the Unix
//! epoch (`toyos_abi::syscall::Stat::mtime`).
//!
//! With no arguments, on the shared boot, it judges `/tmp`: each stamp lies
//! between two `SYS_CLOCK_EPOCH` readings taken around what made it — a write,
//! a create with truncation, a create of a missing file, a truncation — and a
//! later write's stamp is later, which a clock of whole seconds cannot say.
//! Then `/log`: FAT keeps the whole seconds of its flush, read back after a
//! reopen.
//! `write <path>` makes the first judgement on `path` and `read <path>` only
//! prints what it finds there. `undated` runs on a machine whose RTC never
//! answered and never asks the time: a file written there is undated, which
//! std reports as an error and not as 1970.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
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

fn write(path: &str, bytes: &[u8]) {
    let mut f = fs::File::create(path).unwrap_or_else(|e| panic!("create {path}: {e}"));
    f.write_all(bytes).unwrap_or_else(|e| panic!("write {path}: {e}"));
    f.sync_all().unwrap_or_else(|e| panic!("fsync {path}: {e}"));
}

/// Runs `act` and returns `path`'s stamp after it, judged against the wall
/// clock read around it.
fn judged(path: &str, what: &str, act: impl FnOnce()) -> u64 {
    let before = epoch();
    act();
    let after = epoch();
    let stamp = mtime(path);
    // `SYS_CLOCK_EPOCH` is the whole seconds of the clock that stamps, so the
    // stamp is at or past `before` and short of the second after `after`.
    assert!(
        before * NANOS_PER_SEC <= stamp && stamp < (after + 1) * NANOS_PER_SEC,
        "{path} is stamped {stamp} ns by {what}, and the wall clock read {before} s before it \
         and {after} s after it",
    );
    stamp
}

fn write_judged(path: &str, bytes: &[u8]) -> u64 {
    judged(path, "a write", || write(path, bytes))
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

            const TRUNCATED: &str = "/tmp/file-mtime-truncated";
            let created = judged(TRUNCATED, "a create with truncation", || {
                fs::File::create(TRUNCATED).unwrap_or_else(|e| panic!("create {TRUNCATED}: {e}"));
            });

            const MISSING: &str = "/tmp/file-mtime-missing";
            assert!(fs::metadata(MISSING).is_err(), "{MISSING} exists before this creates it");
            judged(MISSING, "a create of a missing file", || {
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(MISSING)
                    .unwrap_or_else(|e| panic!("create {MISSING}: {e}"));
            });

            let resized = judged(TRUNCATED, "a truncation", || {
                let f = OpenOptions::new()
                    .write(true)
                    .open(TRUNCATED)
                    .unwrap_or_else(|e| panic!("reopen {TRUNCATED}: {e}"));
                f.set_len(1).unwrap_or_else(|e| panic!("ftruncate {TRUNCATED}: {e}"));
                f.sync_all().unwrap_or_else(|e| panic!("fsync {TRUNCATED}: {e}"));
            });
            assert!(
                resized > created,
                "{TRUNCATED} was created at {created} ns and a later truncation stamped it \
                 {resized} ns"
            );

            // The flush stamps FAT, which keeps whole seconds and drops an odd one.
            const FAT: &str = "/log/file-mtime";
            let before = epoch();
            write(FAT, b"on FAT");
            let fat = mtime(FAT);
            let after = epoch();
            assert!(
                (before - 1) * NANOS_PER_SEC <= fat
                    && fat <= after * NANOS_PER_SEC
                    && fat % NANOS_PER_SEC == 0,
                "{FAT} reads back {fat} ns after a reopen, and the wall clock read {before} s \
                 before its write and {after} s after",
            );
            fs::remove_file(FAT).unwrap_or_else(|e| panic!("remove {FAT}: {e}"));
            println!("file-mtime: /tmp stamps {first} then {second}, /log {fat}");
        }
        [_, mode] if mode == "undated" => {
            const UNDATED: &str = "/tmp/file-mtime-undated";
            write(UNDATED, b"no clock answered");
            let meta = fs::metadata(UNDATED).unwrap_or_else(|e| panic!("stat {UNDATED}: {e}"));
            match meta.modified() {
                Err(e) if e.kind() == ErrorKind::Unsupported => {}
                other => panic!(
                    "{UNDATED} was written on a machine whose RTC never answered, and its mtime \
                     reads {other:?}"
                ),
            }
            println!("file-mtime: {UNDATED} is undated");
        }
        [_, mode, path] if mode == "write" => {
            let stamp = write_judged(path, b"stamped at its write");
            println!("file-mtime: {path} mtime={stamp}");
        }
        [_, mode, path] if mode == "read" => {
            println!("file-mtime: {path} mtime={}", mtime(path));
        }
        _ => panic!("usage: file_mtime [undated | write <path> | read <path>], got {args:?}"),
    }
}
