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
/// One file more than `MAX_LIST_ENTRIES`: a cwd judged by listing its subtree
/// refuses every spawn from here.
const BIG: &str = "/tmp/spawn-cwd/big";
const BIG_FILES: usize = 16_385;
/// Spawns timed from each cwd, so the figure is an average and not one outlier.
const TIMED: u32 = 8;

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

    // First, so a kernel that grants any of them is seen refusing none of them.
    refusals();

    // The owner's session: the shell's `cd`, then a program it launches.
    said(
        "shell cd, launched",
        Command::new(SHELL).arg("-c").arg(format!("cd {NAMED} && {TOYBOX} pwd")).output(),
        NAMED,
    );
    said(
        "shell -c from current_dir, launched",
        Command::new(SHELL).arg("-c").arg(format!("{TOYBOX} pwd")).current_dir(NAMED).output(),
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

    a_cwd_is_judged_in_its_depth();
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

/// The kernel's answer to one raw spawn of this binary into `cwd`, and the
/// child's exit: 0 is in `cwd`, 3 is somewhere else.
fn spawn_in(cwd: &str) -> Result<i32, SyscallError> {
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
    Ok(code)
}

/// Every refusal is asked before any is asserted, so one that is granted does
/// not hide the rest.
fn refusals() {
    let mut wrong = Vec::new();
    // The same arguments succeed with a directory that exists, so each refusal
    // below is the directory and nothing else.
    let named = spawn_in(NAMED);
    if named != Ok(0) {
        wrong.push(format!("a spawn into {NAMED}: {named:?}, not started in it"));
    }
    for (cwd, want) in [
        (ABSENT, SyscallError::NotFound),
        (FILE, SyscallError::NotFound),
        (SELF, SyscallError::NotFound),
        ("tmp/spawn-cwd/named", SyscallError::InvalidArgument),
        ("", SyscallError::InvalidArgument),
    ] {
        let got = spawn_in(cwd);
        println!("spawn-cwd: a spawn into {cwd:?}: {got:?}");
        if got != Err(want) {
            wrong.push(format!("a spawn into {cwd:?}: {got:?}, not {want:?}"));
        }
    }
    // `SYS_CHDIR` is the same judge, so it refuses the same files.
    for file in [FILE, SELF] {
        if std::env::set_current_dir(file).is_ok() {
            wrong.push(format!("chdir into the file {file} succeeded"));
            std::env::set_current_dir("/").expect("chdir to /");
        }
    }

    match Command::new(SELF).arg("pwd").current_dir(ABSENT).spawn() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => wrong.push(format!("std's word for {ABSENT}: {:?}, not NotFound", e.kind())),
        Ok(mut child) => {
            let _ = child.wait();
            wrong.push(format!("std spawned into {ABSENT}"));
        }
    }
    // init hears the kernel's refusal and answers its client with one.
    if let Ok(mut child) = Command::new(TOYBOX).arg("pwd").current_dir(ABSENT).spawn() {
        let _ = child.wait();
        wrong.push(format!("the launcher started a child in {ABSENT}"));
    }
    assert!(wrong.is_empty(), "refusals that were not:\n{}", wrong.join("\n"));
    println!("spawn-cwd: every refusal arrived under its own name");
}

/// A cwd with more beneath it than one listing may hold is still a cwd, and a
/// spawn from it costs what one from a bare directory does.
fn a_cwd_is_judged_in_its_depth() {
    // Never made by `mkdir`: a directory the VFS carries is answered from its own
    // set, and this one has to be judged by the filesystem its files are on.
    for i in 0..BIG_FILES {
        std::fs::File::create(format!("{BIG}/{i}")).expect("/tmp is writable");
    }
    let from_root = mean_spawn("/");
    let from_named = mean_spawn(NAMED);
    std::env::set_current_dir(BIG).expect("chdir into a directory this made");
    said("own cwd over a large subtree, direct", Command::new(SELF).arg("pwd").output(), BIG);
    let from_big = mean_spawn(BIG);
    std::env::set_current_dir("/").expect("chdir to /");
    println!(
        "spawn-cwd: a spawn and wait, mean of {TIMED}: from / {} us, from {NAMED} {} us, \
         from {BIG} ({BIG_FILES} files) {} us",
        from_root.as_micros(),
        from_named.as_micros(),
        from_big.as_micros(),
    );
    // Removed by name: a listing of this directory is the very thing it outgrew.
    for i in 0..BIG_FILES {
        std::fs::remove_file(format!("{BIG}/{i}")).expect("remove a file this made");
    }
}

/// The mean of [`TIMED`] direct spawns-and-waits from `cwd`.
fn mean_spawn(cwd: &str) -> std::time::Duration {
    let start = std::time::Instant::now();
    for _ in 0..TIMED {
        assert_eq!(spawn_in(cwd), Ok(0), "a timed spawn into {cwd}");
    }
    start.elapsed() / TIMED
}
