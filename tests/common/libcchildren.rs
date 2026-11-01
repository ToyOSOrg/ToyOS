//! libc's children, stage 3 of
//! `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`:
//! each C program under `tests/guest/libc` starts children of its own and
//! exits 0 once `posix_spawn`, the wait family, `kill`, `environ`, `system`,
//! `popen` and `/dev/null` answered as POSIX has them, or with ToyOS's stated
//! departure.
//!
//! Beside them five Rust programs: std's `stdio_named`, where a std child holds
//! the stdio its `Command` named, as a C one does, and `Stdio::null()` is a
//! null handle; `libc_recv_beside_close`, where libc's `recv` outlives a
//! `close` of its descriptor on another thread, its own netstack serving it;
//! `quit_tree`, where a quit asks every process below it that listens and
//! kills the rest; `libc_read_restarts`, where libc's `read` across a
//! `SIGCHLD` handler restarts or answers `EINTR` as `SA_RESTART` says; and
//! `tty_interrupt`, where a shell's Ctrl+C ends its foreground line.
//!
//! On QEMU x86-64 (`tests/images/libccase.toml`), every program in one boot:
//! what they claim is order, completion, contents and counts, which QEMU
//! establishes, and no hardware is in it.

use std::fs;
use std::time::Duration;

use super::{compile, qemu};
use qemu::{BootOptions, QemuInstance};

/// The Rust programs, binaries of `tests/guest/rust-tests`.
const RUST_PROBES: [&str; 5] = ["stdio_named", "libc_recv_beside_close", "quit_tree", "libc_read_restarts", "tty_interrupt"];

/// Each program's bound: `spawn_children_cap` starts and waits for 255.
const BOUND: Duration = Duration::from_secs(300);

pub fn libc_children() -> Result<(), String> {
    let dir = compile::repo_root().join("tests/guest/libc");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".c").map(String::from))
        .collect();
    names.sort();
    let bins: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|name| (name.clone(), compile::link_toyos(&[compile::compile_own_c(&dir, name)], name)))
        .collect();
    let case = compile::repo_root().join("tests/images/libccase.toml");
    let rust_tests = compile::repo_root().join("tests/guest/rust-tests");
    let probes: Vec<(String, Vec<u8>)> =
        RUST_PROBES.iter().map(|name| (name.to_string(), qemu::build_toyos_bin(qemu::SUITE_ARCH, &rust_tests, name))).collect();
    let mut qemu = QemuInstance::boot_with_options(&case, &bins, &probes, BootOptions::default());
    let mut failed = Vec::new();
    let jobs = names
        .iter()
        .map(|name| (name.as_str(), format!("test_c_{name}")))
        .chain(RUST_PROBES.iter().map(|name| (*name, format!("test_rs_{name}"))));
    for (name, job) in jobs {
        let result = qemu.run_test(&job, BOUND);
        match (&result.error, result.exit_code) {
            (None, Some(0)) => {
                for said in result.stdout.lines().filter(|l| l.contains(&format!("{name}: "))) {
                    eprintln!("  [libc_children] {}", said.trim());
                }
            }
            (why, code) => failed.push(format!("{name} ended {code:?} {why:?}:\n{}", result.stdout)),
        }
    }
    match failed.is_empty() {
        true => Ok(()),
        false => Err(failed.join("\n")),
    }
}
