//! A program that dies running from a disk is recorded as its file and the
//! faulting offset, and `symbolize` names the function from them.
//!
//! `/home` rather than `/tmp`: tmpfs has no device under it, so it would prove
//! the mechanism without proving the claim. The claim is about a device.
//!
//! The kernel names no program's frame, so the function's name can only come
//! from the file: a record holding it, or one without the file's path, is the
//! kernel reading a program's symbol table again.

use std::fs;
use std::process::Command;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::log::{LogTail, Record};
use toyos::syscap::SysCap;

const DIR: &str = "/home/disk_backtrace";
const IN_ROOT: &str = "/system/bin/test_rs_disk_backtrace_child";
const ON_DISK: &str = "/home/disk_backtrace/child";
const FRAME: &str = "/home/disk_backtrace/frame";
const NAMER: &str = "/system/bin/symbolize";
/// The child's `#[inline(never)]` function the null read is in.
const FAULTS_IN: &str = "null_deref_run_from_disk";

fn main() {
    let _ = fs::create_dir(DIR);

    let image = fs::read(IN_ROOT)
        .unwrap_or_else(|e| panic!("read {IN_ROOT}: {e}"));
    fs::write(ON_DISK, &image).unwrap_or_else(|e| panic!("write {ON_DISK}: {e}"));
    println!("  copied {} bytes to {ON_DISK}", image.len());

    let mut child = Command::new(ON_DISK)
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {ON_DISK}: {e}"));
    let pid = child.id();
    let status = child.wait().unwrap_or_else(|e| panic!("wait for {ON_DISK}: {e}"));
    assert!(!status.success(), "a child that dereferences null should be killed");

    // Its report is recorded on its own thread before its end is, so it is
    // in the log by the time the wait answers.
    let pc = format!("{ON_DISK}+0x");
    let frame = records_of(pid)
        .into_iter()
        .find(|line| line.contains(&pc))
        .unwrap_or_else(|| panic!("no record of pid {pid} names a frame as {pc}…"));
    println!("  recorded: {frame}");
    assert!(frame.contains(" id=") && !frame.ends_with(" id=-"), "the frame carries no build-id: {frame}");
    assert!(!frame.contains(FAULTS_IN), "the kernel named a program's frame: {frame}");

    fs::write(FRAME, format!("{frame}\n")).unwrap_or_else(|e| panic!("write {FRAME}: {e}"));
    let named = Command::new(NAMER)
        .arg(FRAME)
        .output()
        .unwrap_or_else(|e| panic!("run {NAMER}: {e}"));
    assert!(named.status.success(), "{NAMER} {FRAME}: {:?}", named.status);
    let named = String::from_utf8_lossy(&named.stdout).into_owned();
    println!("  named: {}", named.trim_end());
    assert!(named.contains(FAULTS_IN), "{NAMER} did not name {FAULTS_IN} from {frame}: {named}");

    println!("disk backtrace test passed");
}

/// Every kernel record of `pid` the log still holds, oldest first.
fn records_of(pid: u32) -> Vec<String> {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let mut tail = LogTail::new();
    let mut records = vec![Record::EMPTY; 64];
    let mut lines = Vec::new();
    loop {
        let read = tail.read(&cap, &mut records).expect("read the kernel's records");
        if read.is_empty() {
            return lines;
        }
        lines.extend(read.iter().filter(|r| r.pid == pid).map(|r| r.message().to_owned()));
    }
}
