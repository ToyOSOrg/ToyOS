//! Ctrl+C as a terminal makes it: this probe is the surface an interactive
//! `/system/bin/shell` runs under, types at it through its stdin, and presses
//! Ctrl+C with [`Host::interrupt`], as `/system/bin/terminal` and
//! `/system/bin/console` do.
//!
//! - A pipeline whose first stage, `shell -c cat`, started a child of its own
//!   is ended whole by one Ctrl+C, and the prompt comes back: the shell waits
//!   for every stage, and a stage's end takes what is below it, so nothing of
//!   the line is left.
//! - A shell nested on the line is not ended: Ctrl+C reaches the line it runs
//!   while it runs one, and, at its prompt, the nested shell itself, which
//!   reads it and runs on.
//! - Under the nested shell, a program that reads each interrupt survives
//!   three Ctrl+C, each pressed once it has read the one before, and one that
//!   listens and never reads dies on the second: each Ctrl+C is one quit of
//!   the line it reaches.
//!
//! Which line is the foreground is the surface's (`Lines`, host-tested); what
//! only a guest shows is the shell handing its line over and the quit reaching
//! a tree of real processes. The shell's own output says when a line ended,
//! and the surface's notices when one is held, so no clock decides anything.

use std::io::{Read, Write};
use std::os::toyos::process::CommandExt;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};

use toyos::endow::SVC_LABEL;
use toyos::poller::{Poller, READABLE};
use toyos::surface::{self, Host, Notice};

/// Everything the shell has said and nobody has looked at yet, filled by a
/// thread of its own so the hang ceiling can say it.
static SAID: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static MORE: std::sync::Condvar = std::sync::Condvar::new();

/// The shell under test.
struct Shell {
    stdin: ChildStdin,
}

impl Shell {
    fn type_line(&mut self, line: &str) {
        self.stdin.write_all(format!("{line}\r").as_bytes()).expect("the shell's stdin");
    }

    /// Wait until the shell has said `marker` on a line of its own.
    fn until_said(&mut self, marker: &str) {
        let whole = |said: &str| said.split('\n').rev().skip(1).any(|line| line == marker);
        let mut said = SAID.lock().unwrap();
        while !whole(&said) {
            said = MORE.wait(said).unwrap();
        }
        said.clear();
    }
}

/// Keep what the shell says in [`SAID`].
fn hear(mut stdout: ChildStdout) {
    let mut buf = [0u8; 512];
    while let Ok(n @ 1..) = stdout.read(&mut buf) {
        SAID.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
        MORE.notify_all();
    }
}

/// Serve the surface until `lines` clients hold a foreground line.
fn until_lines(host: &mut Host, held: &mut usize, lines: usize) {
    let poller = Poller::new(Host::POLL_HANDLES);
    while *held != lines {
        while let Some(notice) = host.poll() {
            match notice {
                Notice::Foreground { held: true, .. } => *held += 1,
                Notice::Foreground { held: false, .. } => *held -= 1,
                Notice::Dropped { client, why } => panic!("the surface dropped client {client}: {why}"),
                Notice::LayoutChanged | Notice::Grabbed { .. } | Notice::Released { .. } => {}
            }
        }
        if *held == lines {
            return;
        }
        poller.watch(&host.acceptor_handle(), READABLE, 0);
        for client in host.client_handles() {
            poller.watch(&client, READABLE, 1);
        }
        let mut accept = false;
        poller.wait(1, u64::MAX, |token| accept |= token == 0);
        if accept {
            host.accept();
        }
    }
}

/// Where the probe is, for the line its hang ceiling says.
static STEP: std::sync::Mutex<&str> = std::sync::Mutex::new("starting");

fn step(at: &'static str) {
    *STEP.lock().unwrap() = at;
}

const SELF_PATH: &str = "/system/bin/test_rs_tty_interrupt";

/// Listen, say so, and read three interrupts, saying each.
fn reader() {
    let notice = toyos::quit::notice().expect("a notice");
    notice.listen().expect("listen");
    println!("reader-armed");
    for n in 1..=3 {
        notice.read().expect("read");
        println!("reader-read-{n}");
    }
}

/// Listen, say so, and never read.
fn holder() {
    toyos::quit::notice().expect("a notice").listen().expect("listen");
    println!("holder-armed");
    loop {
        std::thread::park();
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("reader") => return reader(),
        Some("holder") => return holder(),
        Some(other) => panic!("unknown role {other:?}"),
        None => {}
    }
    // A hang ceiling, not a timing claim: a step that never completes ends the
    // probe naming it, rather than leaving it to the harness's ceiling.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(120));
        eprintln!("tty_interrupt: stalled at {}; the shell said:\n{}", STEP.lock().unwrap(), SAID.lock().unwrap());
        std::process::exit(2);
    });
    let (acceptor, connector) = toyos::port::create().expect("a port of our own");
    let mut host = Host::serve(acceptor);
    let mut names = toyos::namespace::build();
    if let Some(own) = toyos::endow::namespace() {
        names = names.keep_all(own);
    }
    let ns = names.add(surface::SERVICE, &connector).finish().expect("the shell's namespace");
    let mut child = Command::new("/system/bin/shell")
        .endow(SVC_LABEL, ns.into())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the shell");
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || hear(stdout));
    let mut shell = Shell { stdin: child.stdin.take().unwrap() };
    let mut held = 0;

    step("the pipeline held");
    shell.type_line("shell -c cat | cat");
    until_lines(&mut host, &mut held, 1);
    step("the pipeline ended");
    assert!(host.interrupt(), "the shell's line was not the foreground");
    until_lines(&mut host, &mut held, 0);
    step("the prompt back");
    shell.type_line("echo pipeline-ended");
    shell.until_said("pipeline-ended");
    until_lines(&mut host, &mut held, 0);
    println!("tty_interrupt: one Ctrl+C ended a pipeline whose stage started a child, and the prompt came back");

    step("the nested shell held");
    shell.type_line("shell");
    until_lines(&mut host, &mut held, 1);
    step("the nested line held");
    shell.type_line("cat");
    until_lines(&mut host, &mut held, 2);
    step("the nested line ended");
    assert!(host.interrupt(), "the nested shell's line was not the foreground");
    until_lines(&mut host, &mut held, 1);
    assert!(host.interrupt(), "the outer shell's line, the nested shell, was not the foreground");
    step("the nested shell alive");
    shell.type_line("echo nested-alive");
    shell.until_said("nested-alive");
    until_lines(&mut host, &mut held, 1);

    step("the reader armed");
    shell.type_line(&format!("{SELF_PATH} reader"));
    shell.until_said("reader-armed");
    until_lines(&mut host, &mut held, 2);
    for n in 1..=3 {
        step("a reader's interrupt read");
        assert!(host.interrupt(), "the reader's line was not the foreground");
        shell.until_said(&format!("reader-read-{n}"));
    }
    step("the reader's line ended");
    until_lines(&mut host, &mut held, 1);
    step("the holder armed");
    shell.type_line(&format!("{SELF_PATH} holder"));
    shell.until_said("holder-armed");
    until_lines(&mut host, &mut held, 2);
    assert!(host.interrupt(), "the holder's line was not the foreground");
    step("the holder killed by the second");
    assert!(host.interrupt(), "the holder's line was not the foreground");
    until_lines(&mut host, &mut held, 1);
    println!("tty_interrupt: a reader outlived three Ctrl+C, and a holder that never read died on the second");
    step("the nested shell gone");
    shell.type_line("exit");
    until_lines(&mut host, &mut held, 0);
    step("the outer shell alive");
    shell.type_line("echo outer-alive");
    shell.until_said("outer-alive");
    println!("tty_interrupt: Ctrl+C ended the nested shell's line and not the nested shell");

    drop(shell);
    let status = child.wait().expect("wait for the shell");
    assert!(status.success(), "the shell ended {status:?} at the end of its input");
    println!("tty_interrupt: the shell ended with its input");
}
