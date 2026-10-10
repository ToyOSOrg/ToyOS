//! `update` run as `ssh <machine> update < image` runs it, on the signed
//! image the harness put on ROOT: launched through this job's launcher, so
//! the supervisor grants it the idle slot, with the image as its standard
//! input. What it says is this job's; whether the slot holds the image is
//! the harness's to read off the disk.

use std::process::{Command, Stdio};

/// Where the harness puts the image (`tests/slotscase`).
const IMAGE: &str = "/system/share/update-test.img";

fn main() {
    let image = std::fs::File::open(IMAGE).expect("update_idle_slot: the image the harness staged");
    let out = Command::new("/system/bin/update")
        .stdin(Stdio::from(image))
        .output()
        .expect("update_idle_slot: update launched");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        println!("update_idle_slot: update said: {line}");
    }
    assert!(out.status.success(), "update_idle_slot: update ended {:?}", out.status);
}
