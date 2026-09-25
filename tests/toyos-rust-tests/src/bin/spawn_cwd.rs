//! A child starts in the directory its spawn names, on both roads to the kernel.
//!
//! `SpawnArgs` carries the child's working directory, and the kernel starts the
//! child there or refuses the spawn by name — it never substitutes the
//! caller's. std's direct spawn states `Command::current_dir` when it is set
//! and this process's own directory when it is not, and init's launcher states
//! the one its client sent.
//!
//! On `tests/netcase`, because the two roads are told apart by configuration
//! there: its test-runner holds a `launcher` connector and declares
//! `/system/bin/toybox` and `/system/bin/shell`, so a spawn of either goes
//! through init, while this binary is declared nowhere and spawns directly.
//!
//! Exit 0 is every arm answering the directory it asked for, and every refusal
//! arriving under its own name.

use std::process::{Command, Output};

use toyos_abi::syscall::{self, SpawnArgs, SyscallError};

const SELF: &str = "/system/bin/test_rs_spawn_cwd";
const TOYBOX: &str = "/system/bin/toybox";
const SHELL: &str = "/system/bin/shell";

/// Two directories, so no arm can pass by answering the one before it.
const NAMED: &str = "/tmp/spawn-cwd/named";
const OWN: &str = "/tmp/spawn-cwd/own";
const ABSENT: &str = "/tmp/spawn-cwd/absent";
/// A file where a directory is asked for, on the volume `SELF` is not.
const FILE: &str = "/tmp/spawn-cwd/file";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("pwd") => {
            println!("{}", std::env::current_dir().expect("a process has a cwd").display());
            return;
        }
        // The raw arm gives it no stdio, so it answers by exit code.
        Some("is") => {
            let here = std::env::current_dir().expect("a process has a cwd");
            let there = args.get(2).map(String::as_str);
            std::process::exit(if here.to_str() == there { 0 } else { 3 });
        }
        _ => {}
    }
    std::fs::create_dir_all(NAMED).expect("/tmp is writable");
    std::fs::create_dir_all(OWN).expect("/tmp is writable");
    std::fs::write(FILE, b"not a directory").expect("/tmp is writable");
    let _ = std::fs::remove_dir(ABSENT);

    // The owner's session: the shell's `cd`, then a program it launches.
    said(
        "shell cd, launched",
        Command::new(SHELL).arg("-c").arg(format!("cd {NAMED} && {TOYBOX} pwd")).output(),
        NAMED,
    );
    said(
        "current_dir, launched",
        Command::new(TOYBOX).arg("pwd").current_dir(NAMED).output(),
        NAMED,
    );
    said(
        "current_dir, direct",
        Command::new(SELF).arg("pwd").current_dir(NAMED).output(),
        NAMED,
    );

    // No `current_dir`: the child starts where its parent is, because std says
    // so on both roads — the kernel has no default to fall back on.
    std::env::set_current_dir(OWN).expect("chdir into a directory this made");
    said("own cwd, launched", Command::new(TOYBOX).arg("pwd").output(), OWN);
    said("own cwd, direct", Command::new(SELF).arg("pwd").output(), OWN);
    said(
        "relative current_dir, direct",
        Command::new(SELF).arg("pwd").current_dir("../named").output(),
        NAMED,
    );
    std::env::set_current_dir("/").expect("chdir to /");

    refusals();
    println!("spawn-cwd: every child started where its spawn said");
}

/// `output` must have started and printed `want` as its whole answer.
fn said(arm: &str, output: std::io::Result<Output>, want: &str) {
    let output = output.unwrap_or_else(|e| panic!("{arm}: the child did not start: {e}"));
    let got = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{arm}: exited {:?}, said {got:?}", output.status);
    assert_eq!(got.trim_end(), want, "{arm}: the child's cwd");
    println!("spawn-cwd: {arm}: {want}");
}

/// The kernel's answer to one raw spawn of this binary into `cwd`.
fn spawn_in(cwd: &str) -> Result<(), SyscallError> {
    let argv = format!("{SELF}\0is\0{cwd}\0");
    let args = SpawnArgs {
        argv_ptr: argv.as_ptr() as u64,
        argv_len: argv.len() as u64,
        slot_map_ptr: 0,
        slot_map_count: 0,
        env_ptr: 0,
        env_len: 0,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        cwd_ptr: cwd.as_ptr() as u64,
        cwd_len: cwd.len() as u64,
    };
    // SAFETY: every pointer names a live local for the length beside it.
    let child = unsafe { syscall::spawn(&args) }?;
    let code = syscall::process_wait(child).expect("wait for a child this spawned");
    syscall::close(child);
    assert_eq!(code, 0, "the child spawned into {cwd:?} is not in it");
    Ok(())
}

fn refusals() {
    // The same arguments succeed with a directory that exists, so each refusal
    // below is the directory and nothing else.
    assert_eq!(spawn_in(NAMED), Ok(()), "a spawn into {NAMED}");
    for (cwd, want) in [
        (ABSENT, SyscallError::NotFound),
        (FILE, SyscallError::NotFound),
        (SELF, SyscallError::NotFound),
        ("tmp/spawn-cwd/named", SyscallError::InvalidArgument),
        ("", SyscallError::InvalidArgument),
    ] {
        assert_eq!(spawn_in(cwd), Err(want), "a spawn into {cwd:?}");
        println!("spawn-cwd: a spawn into {cwd:?} is refused: {want:?}");
    }

    let direct = Command::new(SELF).arg("pwd").current_dir(ABSENT).spawn();
    let refused = direct.map(|_| ()).expect_err("std spawned into a directory that is not there");
    assert_eq!(refused.kind(), std::io::ErrorKind::NotFound, "std's word for {ABSENT}");
    // init hears the kernel's refusal and answers its client with one.
    Command::new(TOYBOX)
        .arg("pwd")
        .current_dir(ABSENT)
        .spawn()
        .map(|_| ())
        .expect_err("the launcher started a child in a directory that is not there");
    println!("spawn-cwd: std and the launcher refuse {ABSENT}");
}
