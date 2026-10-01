//! The boot stick's two partitions, served and written from inside ToyOS.
//!
//! The ESP holds what firmware and the bootloader read. The log partition
//! beside it holds the kernel's own log and exists for one reason: it is typed
//! so that a desktop OS mounts it on plug-in, which an EFI-typed partition is
//! not. Both are FAT32 and neither is found by being FAT32 — the loader names
//! both by unique GUID, fsd serves each off the partition that name finds,
//! and `log_partition_identity` is the gate that says so by moving the name
//! and watching `/log` go absent.
//!
//! Ground truth is the disk image the *device* received, read on the host by
//! implementations that are not fsd's: the `fatfs` crate and
//! `toyos-fat32-check`. The guest's account of a write it made is exactly what
//! is in question, so it cannot also be the evidence; `esp_files` asserts what
//! only a process inside the machine can see, and everything it claims about
//! bytes is checked again here.
//!
//! **Where this stops.** `log_partition_layout` pins the image: type GUID,
//! attribute bits, labels, alignment, and that our own GPT parser finds the
//! partition the ESP names. It does not assert that any operating system
//! *mounts* it. Whether macOS attaches a Basic Data partition is
//! `diskarbitrationd`'s policy, not our contract — it moves between macOS
//! versions and host settings, it would put a volume on the owner's desktop
//! every test run, and it would race concurrent runs. That end of the contract
//! was verified once by hand, on 2026-08-02, and is re-verified when a stick is
//! flashed.
//!
//! The image is built and modified before the boot rather than after, because
//! the host-writes-guest-reads direction has no other staging point: a file the
//! guest itself created and read back would pass with the read path broken.

use std::io::{Cursor, Read};

use fatfs::FsOptions;

/// Read several files out of a FAT volume in one mount. `None` is a file that
/// is not there, which is an assertion in its own right here.
///
/// `fatfs` wants a writable, seekable device even to read, so the volume is
/// copied — once per call, which is why the callers ask for everything they
/// need at once rather than a file at a time.
pub fn read_files(volume: &[u8], paths: &[&str]) -> Result<Vec<Option<Vec<u8>>>, String> {
    let fs = fatfs::FileSystem::new(Cursor::new(volume.to_vec()), FsOptions::new())
        .map_err(|e| format!("the volume does not mount on the host: {e}"))?;
    let root = fs.root_dir();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        match root.open_file(path) {
            Ok(mut file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(|e| format!("reading {path}: {e}"))?;
                out.push(Some(bytes));
            }
            Err(_) => out.push(None),
        }
    }
    Ok(out)
}

/// The other arm of the same table: what the kernel says when both halves are
/// there. `screen_diag_boot` is the gate on it.
///
/// **One declaration, beside [`NO_LOG_ALERT`].** The writer is
/// `report_log_destination` in `kernel/src/main.rs`, whose `(true, true)` arm
/// formats exactly this — a test asserting on a log line reads it from one
/// named declaration that cites its writer, never from a literal copied at the
/// assertion, which is how a hand-copied spelling outlives the kernel's.
pub const LOG_ON_CONSOLE_AND_FILE: &str = "log: this boot is on the console and on /log";
