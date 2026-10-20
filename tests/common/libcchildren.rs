//! libc's children, stage 3 of
//! `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`:
//! each C program under `tests/guest/libc` starts children of its own and
//! exits 0 once `posix_spawn`, the wait family, `kill`, `environ`, `system`
//! and `popen` answered as POSIX has them, or with ToyOS's stated departure.
//!
//! On QEMU x86-64 (`tests/images/libccase.toml`), every program in one boot:
//! what they claim is order, completion, contents and counts, which QEMU
//! establishes, and no hardware is in it.

use std::fs;
use std::time::Duration;

use super::{compile, qemu};
use qemu::{BootOptions, QemuInstance};

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
    let mut qemu = QemuInstance::boot_with_options(&case, &bins, &[], BootOptions::default());
    let mut failed = Vec::new();
    for name in &names {
        let result = qemu.run_test(&format!("test_c_{name}"), BOUND);
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
