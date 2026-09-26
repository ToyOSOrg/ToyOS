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
use std::path::{Path, PathBuf};
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
/// A red run's serial logs are the one part of it read afterwards — by an agent
/// and by the nightly's artifact — so they are copied to [`RED_RUN_SERIAL`] first,
/// replacing the last red run's: megabytes, where the images are gigabytes.
///
/// [`Run::exit`] is the one way out of the suite with a status; returning from
/// `main` drops this as green, and unwinding out of it as red.
pub struct Run(TempDir);

/// Where a red run's `uart-*.log` files are kept, under the repository root.
pub const RED_RUN_SERIAL: &str = "target/red-run-serial";

impl Run {
    /// Take this run's directory; the first one this process makes sweeps what
    /// killed runs left.
    pub fn begin() -> Self {
        let dir = TempDir::new("tests");
        RUN.set(dir.to_path_buf()).expect("one run per process");
        Run(dir)
    }

    /// Leave with `code`, 1 being red, once the run's directory is gone.
    pub fn exit(self, code: i32) -> ! {
        if code == 1 {
            let kept = keep_serial(&self.0).unwrap_or_else(|e| panic!("{e}"));
            eprintln!("[toyos] this red run's serial logs are kept at {}", kept.display());
        }
        drop(self);
        std::process::exit(code)
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // A second panic here would abort and lose the first one's message.
            match keep_serial(&self.0) {
                Ok(kept) => eprintln!("[toyos] this red run's serial logs are kept at {}", kept.display()),
                Err(e) => eprintln!("[toyos] {e}"),
            }
        }
    }
}

/// Copy every `uart-*.log` under `run` to [`RED_RUN_SERIAL`], keeping each
/// one's path below the run, in place of what was there.
fn keep_serial(run: &Path) -> Result<PathBuf, String> {
    let root = super::compile::repo_root();
    let kept = root.join(RED_RUN_SERIAL);
    // Made beside it and moved into place, so what is there is one run's whole
    // set; a second run of this worktree ending red in the same instant finds
    // the rename refused, and says so.
    let fresh = root.join(format!("{RED_RUN_SERIAL}.{}", std::process::id()));
    copy_serial(run, &fresh)?;
    match std::fs::remove_dir_all(&kept) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("remove the last red run's serial logs {}: {e}", kept.display())),
    }
    std::fs::rename(&fresh, &kept)
        .map_err(|e| format!("move {} to {}: {e}", fresh.display(), kept.display()))?;
    Ok(kept)
}

fn copy_serial(dir: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| format!("create {}: {e}", into.display()))?;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        if entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?.is_dir() {
            copy_serial(&path, &into.join(&name))?;
        } else if name.to_str().is_some_and(|n| n.starts_with("uart-") && n.ends_with(".log")) {
            std::fs::copy(&path, into.join(&name))
                .map_err(|e| format!("copy {}: {e}", path.display()))?;
        }
    }
    Ok(())
}
