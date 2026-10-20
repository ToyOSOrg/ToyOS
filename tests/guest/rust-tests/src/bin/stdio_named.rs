//! A std child holds exactly the stdio its `Command` named: a pipe's write end
//! given as stdin refuses the child's read, a pipe whose one reader the parent
//! dropped refuses the child's write, and a child started with a null stdin
//! starts one of its own that inherits it, which ends neither. The first two
//! are the shapes a child's stdio was once seen to hold something else in.
//!
//! `libc_children` runs it on QEMU x86-64 (`tests/common/libcchildren.rs`).

use std::io::{ErrorKind, Read, Write};
use std::process::{Command, Stdio};

const SELF_PATH: &str = "/system/bin/test_rs_stdio_named";

fn reader() -> i32 {
    match std::io::stdin().read(&mut [0u8; 1]) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => 0,
        _ => 4,
    }
}

fn writer() -> i32 {
    let block = [0u8; 4096];
    let mut out = std::io::stdout();
    for _ in 0..1024 {
        match out.write(&block) {
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => return 0,
            Err(_) => return 2,
        }
    }
    3
}

fn spawner() -> i32 {
    match Command::new(SELF_PATH).arg("leaf").stdin(Stdio::inherit()).status() {
        Ok(status) if status.success() => 0,
        _ => 5,
    }
}

fn ended(command: &mut Command, what: &str) {
    let status = command.status().unwrap_or_else(|e| panic!("{what}: spawn: {e}"));
    assert!(status.success(), "{what}: the child ended {status}");
}

fn main() {
    let code = match std::env::args().nth(1).as_deref() {
        Some("reader") => reader(),
        Some("writer") => writer(),
        Some("spawner") => spawner(),
        Some("leaf") => 0,
        Some(other) => panic!("unknown role {other:?}"),
        None => {
            let (_read, write) = std::io::pipe().expect("a pipe");
            ended(Command::new(SELF_PATH).arg("reader").stdin(Stdio::from(write)), "a write end as stdin");

            let mut child = Command::new(SELF_PATH)
                .arg("writer")
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn the writer");
            drop(child.stdout.take());
            let status = child.wait().expect("wait for the writer");
            assert!(status.success(), "a write to a pipe whose reader is gone: the writer ended {status}");

            ended(Command::new(SELF_PATH).arg("spawner").stdin(Stdio::null()), "a child with a null stdin spawning");
            println!("stdio_named: a std child held exactly the stdio its Command named");
            0
        }
    };
    std::process::exit(code);
}
