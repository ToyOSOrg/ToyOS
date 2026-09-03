//! The two refusals the shared-object cache owes, because it never removes an
//! entry: a path whose file changed, and a load past the cache's byte budget.
//!
//! **Both arms need a second process.** `SYS_DLOPEN` answers a name this
//! process already holds out of its own `lib_paths` before the cache is
//! consulted at all, so one process can never ask the cache a second question
//! about one path. Each load below therefore runs in a child of this binary,
//! which reports the answer on its stdout.
//!
//! The stale arm's two libraries export disjoint symbol sets, so the verdict is
//! not a value but a name: `tls_get_label` exists only in the first, and a
//! kernel that resolved it after the file became the second served an image the
//! file no longer holds. `tests/common/storage.rs::so_cache_refusals` boots
//! this with `so-cache-tiny` and reads the replaced file's bytes off the NVMe
//! image afterwards.

use std::process::{Command, Stdio};

use toyos_abi::syscall::{self, SyscallError};

/// Mirrored in `tests/common/storage.rs::so_cache_refusals`, which reads this
/// path's bytes off the device once the guest is down.
const STALE: &str = "/home/so-cache-stale.so";
/// The first library written to [`STALE`]; exports `tls_get_label`.
const FIRST: &str = "/lib/libtls_lib.so";
/// The second; a different library, a different size, and no `tls_get_label`.
const SECOND: &str = "/lib/libtls_dlopen_lib.so";
/// A symbol `FIRST` exports and `SECOND` does not.
const ONLY_IN_FIRST: &[u8] = b"tls_get_label";

/// `TINY_BUDGET_BYTES` is 8 MiB and each copy below is a 2 MiB image, so a
/// refusal is due within this many. Larger than the four that fit, so a kernel
/// that never refuses runs out of attempts and says so rather than looping.
const BUDGET_ATTEMPTS: usize = 12;

const SELF_PATH: &str = "/bin/test_rs_so_cache_policy";

fn main() {
    match std::env::args().nth(1) {
        Some(path) => load_and_report(&path),
        None => test(),
    }
}

/// Both arms run whatever the first one answers, because on a kernel with
/// neither refusal both are red and a run that stopped at the first would
/// report one control and hide the other.
fn test() {
    let arms = [
        ("stale", a_changed_file_is_refused()),
        ("budget", the_budget_is_refused()),
    ];
    let mut failed = false;
    for (name, outcome) in arms {
        match outcome {
            Ok(said) => println!("  {name}: {said}"),
            Err(why) => {
                println!("  {name} FAILED: {why}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    println!("the shared-object cache refuses a stale path and a full budget");
}

/// Write one library, load it, write a different one over the same path, and
/// load again. The second load must be refused: the first image is mapped into
/// the child that loaded it and nothing here can take it back, so serving it
/// again is a lie and reloading would map the library twice.
fn a_changed_file_is_refused() -> Result<String, String> {
    let only = String::from_utf8_lossy(ONLY_IN_FIRST).into_owned();
    copy(FIRST, STALE);
    let first = load_in_child(STALE);
    if !(first.contains("LOADED") && first.contains("SYMBOL-FOUND")) {
        return Err(format!("the first load of {STALE} did not resolve {only}: {first}"));
    }

    copy(SECOND, STALE);
    let second = load_in_child(STALE);
    if second.contains("SYMBOL-FOUND") {
        return Err(format!(
            "{STALE} now holds {SECOND}, which exports no {only} — a kernel that resolved it \
             served the image {FIRST} left in the cache: {second}"
        ));
    }
    if !second.contains("REFUSED NotSupported") {
        return Err(format!(
            "the load of a path whose file changed was not refused by name: {second}"
        ));
    }
    Ok(format!("{STALE} became {SECOND} and the second load was refused"))
}

/// Distinct paths are distinct entries, so copies of one library fill the
/// budget as surely as distinct libraries would. The refusal is
/// `ResourceExhausted` and it arrives before the attempts run out.
fn the_budget_is_refused() -> Result<String, String> {
    for attempt in 0..BUDGET_ATTEMPTS {
        let path = format!("/home/so-cache-fill-{attempt}.so");
        copy(FIRST, &path);
        let said = load_in_child(&path);
        if said.contains("REFUSED ResourceExhausted") {
            return Ok(format!("refused at copy {attempt} of {BUDGET_ATTEMPTS}"));
        }
        if !said.contains("LOADED") {
            return Err(format!("copy {attempt} neither loaded nor refused: {said}"));
        }
    }
    Err(format!(
        "{BUDGET_ATTEMPTS} distinct 2 MiB images entered the cache unrefused — this kernel \
         holds no byte budget over it at all"
    ))
}

fn copy(from: &str, to: &str) {
    let bytes = std::fs::read(from).unwrap_or_else(|e| panic!("read {from}: {e}"));
    std::fs::write(to, &bytes).unwrap_or_else(|e| panic!("write {to}: {e}"));
}

/// One `dlopen` in a child, and what it said.
fn load_in_child(path: &str) -> String {
    let out = Command::new(SELF_PATH)
        .arg(path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn a loader for {path}: {e}"))
        .wait_with_output()
        .unwrap_or_else(|e| panic!("wait for the loader of {path}: {e}"));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The child: load `path`, say what happened, and — on a load that worked —
/// whether the symbol only the first library carries is there.
fn load_in_this_process(path: &str) {
    match syscall::dl_open(path.as_bytes()) {
        Ok(handle) => {
            println!("LOADED");
            if unsafe { syscall::dl_sym(handle, ONLY_IN_FIRST) }.is_ok() {
                println!("SYMBOL-FOUND");
            }
        }
        Err(SyscallError::NotSupported) => println!("REFUSED NotSupported"),
        Err(SyscallError::ResourceExhausted) => println!("REFUSED ResourceExhausted"),
        Err(other) => println!("REFUSED {other:?}"),
    }
}

fn load_and_report(path: &str) -> ! {
    load_in_this_process(path);
    std::process::exit(0)
}
