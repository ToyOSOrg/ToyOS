//! A program running from a disk gets a backtrace with names in it.
//!
//! `/home` rather than `/tmp`: tmpfs has no device under it, so it would prove
//! the mechanism without proving the claim. The claim is about a device.

use std::fs;
use std::process::Command;

const DIR: &str = "/home/disk_backtrace";
const IN_ROOT: &str = "/bin/test_rs_disk_backtrace_child";
const ON_DISK: &str = "/home/disk_backtrace/child";

fn main() {
    let _ = fs::create_dir(DIR);

    let image = fs::read(IN_ROOT)
        .unwrap_or_else(|e| panic!("read {IN_ROOT}: {e}"));
    fs::write(ON_DISK, &image).unwrap_or_else(|e| panic!("write {ON_DISK}: {e}"));
    println!("  copied {} bytes to {ON_DISK}", image.len());

    // The verdict is on the serial, not here: `check_disk_backtrace` reads the
    // kernel's SEGFAULT report out of this test's own capture window. What this
    // side proves is that the child ran from the disk at all and died the way
    // it was supposed to — without which the serial check would be asserting on
    // a report that never happened.
    let status = Command::new(ON_DISK)
        .status()
        .unwrap_or_else(|e| panic!("spawn {ON_DISK}: {e}"));
    assert!(!status.success(), "a child that dereferences null should be killed");

    println!("  PASS: {ON_DISK} faulted (exit={})", status.code().unwrap_or(-1));
    println!("disk backtrace test passed");
}
