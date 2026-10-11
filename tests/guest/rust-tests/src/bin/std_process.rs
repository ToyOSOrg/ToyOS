use std::fs;
use std::process::Command;

fn main() {
    test_status();
    test_output();
    test_output_stderr();
    test_output_closes_stdin();
    test_exit_code();
    test_spawn_and_wait();
    test_piped_stdin();
    test_write_to_a_gone_reader();
    println!("all process tests passed");
}

fn test_status() {
    let status = Command::new("/system/bin/echo")
        .arg("hi")
        .status()
        .expect("failed to run echo");
    assert!(status.success(), "echo should succeed");
    println!("  Command::status(): ok");
}

fn test_output() {
    let output = Command::new("/system/bin/echo")
        .arg("hello world")
        .output()
        .expect("failed to run echo");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "hello world");
    println!("  Command::output(): ok");
}

/// Both of a child's streams, from one `output()` call.
///
/// `Output::stderr` came back empty whatever the child wrote, so a caller could
/// not tell "the child said nothing" from "we did not look" — and this is also
/// the only place two pipes are drained at once, which is a thread per call.
fn test_output_stderr() {
    let path = "/tmp/std_process_stderr";
    fs::write(path, b"on stdout\n").expect("write the file cat reads");

    let output = Command::new("/system/bin/cat")
        .args([path, "/nonexistent_file"])
        .output()
        .expect("failed to run cat");
    assert!(!output.status.success(), "cat of a missing file should fail");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "on stdout", "stdout was {stdout:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/nonexistent_file: file not found"),
        "stderr was {stderr:?}"
    );
    println!("  Command::output() stderr: ok");
}

/// A child reading stdin to EOF must get one: `output()` inherits nothing, so
/// the parent's write end has to go before the read of stdout begins.
fn test_output_closes_stdin() {
    let output = Command::new("/system/bin/cat").output().expect("failed to run cat");
    assert!(output.status.success(), "cat on an empty stdin should succeed");
    assert!(output.stdout.is_empty(), "cat echoed {:?}", output.stdout);
    println!("  Command::output() closes stdin: ok");
}

fn test_exit_code() {
    let status = Command::new("/system/bin/cat")
        .arg("/nonexistent_file")
        .status()
        .expect("failed to run cat");
    assert!(!status.success(), "cat nonexistent file should fail");
    println!("  exit code (failure): ok");
}

fn test_spawn_and_wait() {
    let mut child = Command::new("/system/bin/echo")
        .arg("spawned")
        .spawn()
        .expect("failed to spawn echo");
    let status = child.wait().expect("failed to wait");
    assert!(status.success());
    println!("  spawn + wait: ok");
}

fn test_piped_stdin() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("/system/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn cat");

    child.stdin.take().unwrap().write_all(b"piped input\n").unwrap();
    let output = child.wait_with_output().expect("failed to wait");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "piped input");
    println!("  piped stdin/stdout: ok");
}

/// A write into a pipe whose reader has exited is `BrokenPipe` — the word
/// POSIX spells `EPIPE` and this machine's libc already sets.
fn test_write_to_a_gone_reader() {
    use std::io::{ErrorKind, Write};
    use std::process::Stdio;

    let mut child = Command::new("/system/bin/echo")
        .arg("gone")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn echo");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let status = child.wait().expect("failed to wait");
    assert!(status.success(), "echo exited {status:?}");

    // Enough writes to fill any ring: a buffered success is not a live reader.
    let mut last = None;
    for _ in 0..64 {
        match stdin.write(&[b'x'; 4096]) {
            Ok(_) => continue,
            Err(e) => {
                last = Some(e);
                break;
            }
        }
    }
    let err = last.expect("64 writes into a pipe with no reader all succeeded");
    assert_eq!(
        err.kind(),
        ErrorKind::BrokenPipe,
        "a write into a pipe whose reader has exited answered {err:?}"
    );
    println!("  write to a gone reader: ok");
}
