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
use std::sync::OnceLock;

use toyos_tmpdir::TempDir;

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

/// This thread's scratch directory, created if it is not there, inside the
/// run's.
pub fn dir() -> PathBuf {
    let mut dir = RUN.get().expect("the suite's `Run::begin` comes before any scratch").clone();
    if let Some(index) = LANE.with(Cell::get) {
        dir.push(format!("lane-{index}"));
        // Not `create_dir_all`: a run directory that is gone stays gone, and a
        // lane asked for after it would otherwise be made outside any holder.
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("create the test directory {}: {e}", dir.display()),
        }
    }
    dir
}

/// The run's directory, set once by [`Run::begin`].
static RUN: OnceLock<PathBuf> = OnceLock::new();

/// This run's hold on its scratch directory, from before the first boot to the
/// exit: every image, boot log, screendump and socket of every lane is under
/// it, and it is gone when the run is, green or red (`toyos_tmpdir` is the
/// policy, and what reclaims the directory of a run that was killed).
///
/// [`Run::exit`] is the one way out of the suite with a status; returning from
/// `main` or unwinding out of it drops this.
pub struct Run(TempDir);

impl Run {
    /// Take this run's directory; the first one this process makes sweeps what
    /// killed runs left.
    pub fn begin() -> Self {
        let dir = TempDir::new("tests");
        RUN.set(dir.to_path_buf()).expect("one run per process");
        Run(dir)
    }

    /// Remove the run's directory, then leave with `code`.
    pub fn exit(self, code: i32) -> ! {
        drop(self);
        std::process::exit(code)
    }
}
