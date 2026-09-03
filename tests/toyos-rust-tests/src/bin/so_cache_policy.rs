//! The two refusals the shared-object cache owes, because it never removes an
//! entry: a path whose file changed, and a load past its byte budget.
//! `tests/common/storage.rs::so_cache_refusals` boots and judges this.
//!
//! **Both arms need a second process.** `SYS_DLOPEN` answers a name this
//! process already holds out of its own `lib_paths` before the cache is
//! consulted at all, so one process can never ask the cache twice about one
//! path. Each load runs in a child, which reports on its stdout.

use std::process::{Command, Stdio};

use toyos_abi::syscall::{self, SyscallError};

/// Mirrored in `tests/common/storage.rs::so_cache_refusals`, which reads this
/// path's bytes off the device once the guest is down.
const STALE: &str = "/home/so-cache-stale.so";
const FIRST: &str = "/lib/libtls_lib.so";
const SECOND: &str = "/lib/libtls_dlopen_lib.so";
/// A symbol `FIRST` exports and `SECOND` does not, so the verdict is a name and
/// not a value.
const ONLY_IN_FIRST: &[u8] = b"tls_get_label";

/// The budget is 8 MiB and each copy is a 2 MiB image, so a refusal is due well
/// inside this — and a kernel that never refuses says so instead of looping.
const BUDGET_ATTEMPTS: usize = 12;

const SELF_PATH: &str = "/bin/test_rs_so_cache_policy";

fn main() {
    match std::env::args().nth(1) {
        Some(path) => load_and_report(&path),
        None => test(),
    }
}

/// Both arms run whatever the first answers: on a kernel with neither refusal
/// both are red, and stopping at the first would hide one control.
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

/// One library, loaded; a different one over the same path, loaded again. The
/// second must be refused: the first image is mapped into the child that loaded
/// it, so serving it again is a lie and reloading would map the library twice.
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
/// budget as surely as distinct libraries would.
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

/// The child: load `path` and say what happened.
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
