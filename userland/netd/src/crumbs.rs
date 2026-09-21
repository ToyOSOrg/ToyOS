//! `crate::EXIT_WITH_CRUMBS`'s trail: `toyos_i219::crumbs` lines appended to a
//! file on the log volume, each one flushed to the device before the step it
//! names is taken.
//!
//! **`sync_all` is the whole claim.** `SYS_FSYNC` reaches the device's own
//! cache flush, which is what `/system/bin/logd` rests its durability word on,
//! so a line this returned from is on the stick whatever the machine does next.
//!
//! **A crumb that cannot be made durable ends this process.** A trail with a
//! hole in it names the wrong step, and the boot it was flashed for is better
//! read as one whose instrument failed than as one that answered.

use std::cell::{Cell, RefCell};
use std::fs::{File, OpenOptions};
use std::io::Write;

use toyos_i219::crumbs::{Line, Step, Trail};

/// Beside `/system/bin/logd`'s files and not one of them:
/// `toyos_wallclock::classify` is what logd may delete and what the metal
/// loop reads as the boot's log, and it does not recognise this name.
pub const PATH: &str = "/log/crumbs.txt";

pub struct Stick {
    file: RefCell<File>,
    next: Cell<u32>,
    /// When the last line came back durable.
    synced: Cell<u64>,
}

impl Stick {
    /// Appending, so a second boot off one flash adds to the first's trail
    /// instead of replacing the only record of how it ended.
    pub fn open() -> Self {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(PATH)
            .unwrap_or_else(|why| panic!("netd: {PATH} would not open: {why}"));
        Self { file: RefCell::new(file), next: Cell::new(0), synced: Cell::new(0) }
    }
}

impl Trail for Stick {
    fn crumb(&self, step: Step) {
        let seq = self.next.replace(self.next.get() + 1);
        let at = toyos_abi::syscall::clock_nanos();
        let line = format!("{}\n", Line { seq, at, synced: self.synced.get(), step });
        let mut file = self.file.borrow_mut();
        file.write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
            .unwrap_or_else(|why| panic!("netd: crumb {seq} `{step}` is not on {PATH}: {why}"));
        self.synced.set(toyos_abi::syscall::clock_nanos());
    }
}
