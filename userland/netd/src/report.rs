//! `crate::EXIT_WITH_LEASE`'s report: `toyos_i219::lease` lines appended to a
//! file on the log volume, each flushed to the device before the process goes
//! on.
//!
//! **`sync_all` is the whole claim**, as it is for `crate::crumbs`: `SYS_FSYNC`
//! reaches the device's own cache flush, so a line this returned from is on
//! the stick whatever the machine does next. A line that cannot be made
//! durable ends the process: a report with a hole in it says the wrong thing.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::Instant;

use toyos_i219::lease::{Event, Line};

/// Beside `/system/bin/logd`'s files and not one of them, for the reason
/// `crate::crumbs::PATH` is.
pub const PATH: &str = "/log/lease.txt";

pub struct Report {
    file: RefCell<File>,
    /// When this process started, which every line's milliseconds count from.
    began: Instant,
}

impl Report {
    /// Appending, so a second boot off one flash adds to the first's report
    /// instead of replacing the only record of how it went.
    pub fn open(began: Instant) -> Self {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(PATH)
            .unwrap_or_else(|why| panic!("netd: {PATH} would not open: {why}"));
        Self { file: RefCell::new(file), began }
    }

    pub fn say(&self, event: Event<'_>) {
        let line = Line { ms: self.began.elapsed().as_millis() as u64, event };
        let text = format!("{line}\n");
        let mut file = self.file.borrow_mut();
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .unwrap_or_else(|why| panic!("netd: `{line}` is not on {PATH}: {why}"));
    }
}
