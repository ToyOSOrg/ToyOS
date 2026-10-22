//! A program spawned from a file in the kernel's `/tmp` runs the file's bytes
//! as they were at the spawn: the file overwritten with `hlt` and deleted the
//! moment the spawn returns, the child reads every page of its data after
//! that, at each page's first touch, and finds the bytes it was built with.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

const SELF: &str = "/system/bin/test_rs_spawn_from_tmp";
const COPY: &str = "/tmp/spawn_from_tmp";
const CHILD: &str = "child";

/// Read-only data no page of which the child touches before it is told to.
const BYTE: u8 = 7;
static DATA: [u8; 256 * 1024] = [BYTE; 256 * 1024];

fn child() {
    let mut go = [0u8];
    std::io::stdin().read_exact(&mut go).expect("the parent's go-ahead");
    let data = std::hint::black_box(&DATA);
    // SAFETY: each index is inside `DATA`, a static that lives for the program.
    let wrong = (0..data.len()).filter(|&i| unsafe { core::ptr::read_volatile(&data[i]) } != BYTE).count();
    if wrong != 0 {
        eprintln!("spawn_from_tmp: {wrong} of {} data bytes are not the ones the program was built with", data.len());
        std::process::exit(1);
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        return child();
    }
    let bytes = std::fs::read(SELF).expect("read this program");
    std::fs::write(COPY, &bytes).expect("copy this program into /tmp");
    let mut child = Command::new(COPY).arg(CHILD).stdin(Stdio::piped()).spawn().expect("spawn the copy in /tmp");
    std::fs::write(COPY, vec![0xF4; bytes.len()]).expect("overwrite the copy with hlt");
    std::fs::remove_file(COPY).expect("delete the copy");
    child.stdin.take().expect("the child's stdin").write_all(&[1]).expect("tell the child to read its data");
    let status = child.wait().expect("wait for the child");
    assert!(status.success(), "the child spawned from /tmp, its file overwritten and deleted: {status}");
    println!("spawn_from_tmp: a child spawned from /tmp read its data as it was at the spawn");
    println!("spawn_from_tmp: PASS");
}
