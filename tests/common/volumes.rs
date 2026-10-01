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
