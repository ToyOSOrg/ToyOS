//! A paged object mapped by another process, on both architectures:
//! `mapprobe`, launched as its row's pager, maps its objects in children of
//! its own, answers or withholds each fault and judges each child by its end
//! (`tests/guest/mapprobe`). The probe says each claim as it holds and exits
//! 0 once every one did.
//!
//! Needs the guest: the faults, waits, revocations and refusals are a booted
//! kernel's, between two processes.

use std::os::toyos::process::CommandExt;
use std::process::Command;

fn main() {
    let status = Command::new("/system/bin/mapprobe")
        .independent()
        .status()
        .expect("the mapping probe launches: its image's test-runner starts it");
    assert!(status.success(), "the mapping probe ended {status:?}");
    println!("mapped_file: a mapped paged object follows its pager");
}
