//! One scratch directory per thread that boots guests.
//!
//! Every scratch file in this harness is named for what it is — `esp-boot.img`,
//! `usb-gate-512.img`, the size-keyed NVMe image `boot_with_options` makes — and
//! not for the boot that uses it, because until the suite ran serially there was
//! only ever one boot. Two guests handed one path is the defect that killed the
//! first consolidation
//! attempt, and it is not a defect a per-file fix closes: the next test to add a
//! staged image gets the shared directory by default and nothing says otherwise.
//!
//! So the *directory* carries the answer. A worker enters its lane once and
//! every path derived from [`dir`] afterwards is that worker's alone, including
//! ones written after this was.
//!
//! A lane is not a place state carries between tests: the NVMe scratch image is
//! remade blank for every boot that names none (`QemuInstance::boot_with_options`),
//! so no test can depend on, or be broken by, whichever test ran before it.

use std::cell::Cell;
use std::path::PathBuf;

thread_local! {
    /// `None` on any thread that never entered one, which is the suite's own
    /// thread and every serial-tail test on it.
    static LANE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Name this thread's lane for the rest of its life.
///
/// Called once by each worker before it takes any work. A lane is a property of
/// the *thread* rather than of the boot, because the paths it decides are
/// derived deep inside test bodies that have no business knowing the suite runs
/// wide.
pub fn enter(index: usize) {
    LANE.with(|lane| {
        assert!(lane.get().is_none(), "a worker entered two lanes");
        lane.set(Some(index));
    });
}

/// This thread's scratch directory, created if it is not there.
pub fn dir() -> PathBuf {
    let mut dir = toyos_build::scratch::run_dir(&std::env::temp_dir(), std::process::id());
    if let Some(index) = LANE.with(Cell::get) {
        dir.push(format!("lane-{index}"));
    }
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("create the test directory {}: {e}", dir.display()));
    dir
}

/// This run's hold on its scratch directory, from before the first boot to the
/// exit: `toyos_build::scratch` is the policy.
///
/// [`Run::exit`] is the one way out of the suite with a status; returning from
/// `main` drops this as a green run, and unwinding out of it as a red one.
pub struct Run;

impl Run {
    /// Sweep what earlier runs left, and take this run's directory.
    pub fn begin() -> Self {
        let tmp = std::env::temp_dir();
        let removed =
            toyos_build::scratch::sweep(&tmp, toyos_build::scratch::alive, std::time::SystemTime::now());
        if !removed.is_empty() {
            eprintln!("[toyos] removed {} old run director(ies) from {}", removed.len(), tmp.display());
        }
        Run
    }

    /// Leave with `code`: 1 is red and keeps the scratch, anything else removes it.
    pub fn exit(self, code: i32) -> ! {
        end(code == 1);
        std::mem::forget(self);
        std::process::exit(code)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        end(std::thread::panicking());
    }
}

fn end(red: bool) {
    let dir = toyos_build::scratch::run_dir(&std::env::temp_dir(), std::process::id());
    if let Some(kept) = toyos_build::scratch::finish(&dir, red) {
        eprintln!("[toyos] this run's boot logs, screendumps and images are kept at {}", kept.display());
    }
}
