//! A quit reaches a process and everything below it: a process that listens
//! to its notice reads the reason and ends as it chooses, and one that never
//! listened is killed with everything below it, listening or not.
//!
//! The decisions are `toyos_proclife::quit`'s and its interleavings are host
//! tests; what only a guest shows is the walk over real processes, the notice
//! each is started holding, the read that blocks until a quit fills it and the
//! watch that fires, and the kill a second quit makes of a reason left unread.
//!
//! Every child reports on the one pipe the test reads, a line per step, and
//! the test orders each quit after the lines that say what it is asking: no
//! clock decides anything.

use std::io::{BufRead, BufReader, Read};
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process::ExitStatusExt;
use std::process::{Child, ChildStdout, Command, Stdio};

use toyos::poller::{Poller, READABLE};
use toyos::process::Process;
use toyos::quit::Reason;
use toyos_abi::end::End;
use toyos_abi::handle::Rights;
use toyos_abi::syscall::SyscallError;
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_quit_tree";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["listener", depth, how] => listener(depth.parse().expect("a depth"), how),
        ["deaf"] => deaf(),
        ["reader", times] => reader(times.parse().expect("a count")),
        ["holder"] => holder(),
        [] => test(),
        other => panic!("unknown role {other:?}"),
    }
}

fn test() {
    two_generations_that_listen_read_the_reason("read", Reason::Interrupt);
    two_generations_that_listen_read_the_reason("watch", Reason::HangUp);
    one_that_never_listened_is_killed_with_its_listening_child();
    a_second_quit_kills_a_reason_left_unread();
    one_that_reads_each_reason_is_asked_again();
    a_quit_needs_manage_and_an_ended_process_is_no_refusal();
    println!("quit_tree: a quit asks every process below it that listens, and kills the rest");
}

/// The reason's word in the lines the children say.
fn word(reason: Reason) -> &'static str {
    match reason {
        Reason::Interrupt => "interrupt",
        Reason::HangUp => "hang-up",
        Reason::Terminate => "terminate",
    }
}

/// `role` of this binary, its stdout a pipe this test reads and its stdin
/// one it holds, so a role that reads stdin blocks until the test lets it go.
fn start(role: &[&str]) -> (Child, BufReader<ChildStdout>) {
    let mut child = Command::new(SELF_PATH)
        .args(role)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {role:?}: {e}"));
    let out = BufReader::new(child.stdout.take().expect("piped"));
    (child, out)
}

/// The next lines the children said, which must be `expected` in any order:
/// two processes share the pipe.
fn said(out: &mut BufReader<ChildStdout>, expected: &[&str]) {
    let mut lines = Vec::new();
    for _ in expected {
        let mut line = String::new();
        out.read_line(&mut line).expect("a child's line");
        lines.push(line.trim_end().to_string());
    }
    lines.sort();
    let mut expected: Vec<&str> = expected.to_vec();
    expected.sort();
    assert_eq!(lines, expected, "the children said something else");
}

/// Every line still to come, to the pipe's end.
fn rest(mut out: BufReader<ChildStdout>) -> String {
    let mut text = String::new();
    out.read_to_string(&mut text).expect("the children's lines");
    text
}

fn quit(child: &Child, reason: Reason) -> Result<(), SyscallError> {
    toyos_abi::syscall::process_quit(RawHandle(child.as_raw_handle()), reason)
}

fn end(child: &mut Child) -> End {
    let status = child.wait().expect("wait");
    End::from_word(status.into_raw()).expect("an end the kernel makes")
}

/// A listener with a listener below it, both listening by `how`: a read, or
/// a watch armed on the notice. Each reads the reason and exits 0, the parent
/// once its child has.
fn two_generations_that_listen_read_the_reason(how: &str, reason: Reason) {
    let (mut child, mut out) = start(&["listener", "1", how]);
    said(&mut out, &["d0 armed", "d1 armed"]);
    quit(&child, reason).expect("quit");
    let text = rest(out);
    for depth in 0..2 {
        let line = format!("d{depth} read {}", word(reason));
        assert!(text.lines().any(|l| l == line), "{line:?} was not said, by {how}:\n{text}");
    }
    assert_eq!(end(&mut child), End::Exited(0), "the parent listener, by {how}");
    println!("quit_tree: two generations listening by {how} each read {} and exited 0", word(reason));
}

/// A process that never listened, with a listening child: the quit kills it,
/// its end takes the child, and the child reads nothing.
fn one_that_never_listened_is_killed_with_its_listening_child() {
    let (mut child, mut out) = start(&["deaf"]);
    said(&mut out, &["d0 armed", "deaf armed"]);
    quit(&child, Reason::Terminate).expect("quit");
    assert_eq!(end(&mut child), End::Killed, "a process that never listened");
    let text = rest(out);
    assert!(!text.contains("read"), "a child below a process that never listened read a reason:\n{text}");
    println!("quit_tree: one that never listened is killed, and its listening child with it, reading nothing");
}

/// A process listening that never reads: the first quit leaves it the
/// reason, and the second kills it.
fn a_second_quit_kills_a_reason_left_unread() {
    let (mut child, mut out) = start(&["holder"]);
    said(&mut out, &["holder armed"]);
    quit(&child, Reason::Interrupt).expect("the first quit");
    assert_eq!(child.try_wait().expect("try_wait"), None, "the first quit killed a process that listens");
    quit(&child, Reason::Interrupt).expect("the second quit");
    assert_eq!(end(&mut child), End::Killed, "the second quit, the first unread");
    println!("quit_tree: a second quit kills a process holding the first unread");
}

/// A process that reads each reason survives each quit, asked once per quit.
fn one_that_reads_each_reason_is_asked_again() {
    const TIMES: u32 = 3;
    let (mut child, mut out) = start(&["reader", &TIMES.to_string()]);
    said(&mut out, &["reader armed"]);
    for n in 1..=TIMES {
        quit(&child, Reason::Interrupt).expect("quit");
        said(&mut out, &[&format!("reader read {n}")]);
    }
    assert_eq!(end(&mut child), End::Exited(0), "a reader of every reason");
    println!("quit_tree: one that reads each reason is asked {TIMES} times and lives");
}

/// A handle narrowed to `WAIT` asks nothing, and the kill's own right asks; a
/// quit of a process already ended is answered as a kill of one is.
fn a_quit_needs_manage_and_an_ended_process_is_no_refusal() {
    let (mut child, mut out) = start(&["holder"]);
    said(&mut out, &["holder armed"]);
    // SAFETY: the child's handle, given back by `into_raw` below before
    // anything else could answer for it.
    let own = unsafe { Process::from_raw(RawHandle(child.as_raw_handle())) };
    let narrowed = own.narrowed(Rights::WAIT).expect("a narrowed duplicate");
    let _ = own.into_raw();
    assert_eq!(narrowed.quit(Reason::Interrupt), Err(SyscallError::PermissionDenied), "a quit without MANAGE");
    let ends = toyos_abi::syscall::pipe().expect("a pipe");
    assert_eq!(
        toyos_abi::syscall::process_quit(ends.read, Reason::Interrupt),
        Err(SyscallError::PermissionDenied),
        "a quit of a handle to no process, as a peer may send one: no such handle carries MANAGE, and the caller lives",
    );
    // SAFETY: no type holds this number.
    unsafe { toyos_abi::syscall::close(ends.read) };
    // SAFETY: no type holds this number.
    unsafe { toyos_abi::syscall::close(ends.write) };
    child.kill().expect("kill");
    assert_eq!(end(&mut child), End::Killed);
    assert_eq!(quit(&child, Reason::Interrupt), Ok(()), "a quit of an ended process");
    println!("quit_tree: a quit without MANAGE or of no process is refused, and one of an ended process answers Ok");
}

fn notice() -> &'static toyos::quit::Notice {
    toyos::quit::notice().expect("every process is started holding its notice")
}

/// Listen by `how`, start a listener `depth - 1` below, say so, then read one
/// reason and exit once the child has.
fn listener(depth: u32, how: &str) {
    let poller = Poller::new(1);
    match how {
        "read" => notice().listen().expect("listen"),
        "watch" => {
            poller.watch(notice(), READABLE, 0);
            // Submitted with nothing waited for: the watch is armed from here.
            poller.wait(0, 0, |_| {});
        }
        other => panic!("listen by {other:?}"),
    }
    let mut below = (depth > 0).then(|| {
        Command::new(SELF_PATH)
            .args(["listener", &(depth - 1).to_string(), how])
            .stdin(Stdio::null())
            .spawn()
            .expect("spawn the listener below")
    });
    println!("d{depth} armed");
    let reason = match how {
        "read" => notice().read().expect("read"),
        _ => {
            poller.wait(1, u64::MAX, |_| {});
            notice().try_read().expect("try_read").expect("the watch fired with no reason in the notice")
        }
    };
    println!("d{depth} read {}", word(reason));
    let code = below.as_mut().map_or(0, |child| match end(child) {
        End::Exited(0) => 0,
        other => panic!("the listener below ended {other:?}"),
    });
    std::process::exit(code);
}

/// Never listens, holds a listener below it, and waits on stdin.
fn deaf() {
    let _below = Command::new(SELF_PATH)
        .args(["listener", "0", "read"])
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn the listener below");
    println!("deaf armed");
    let _ = std::io::stdin().read(&mut [0u8]);
    std::process::exit(1);
}

/// Listens, says so, and reads `times` reasons.
fn reader(times: u32) {
    notice().listen().expect("listen");
    println!("reader armed");
    for n in 1..=times {
        notice().read().expect("read");
        println!("reader read {n}");
    }
}

/// Listens, says so, and never reads.
fn holder() {
    notice().listen().expect("listen");
    println!("holder armed");
    let _ = std::io::stdin().read(&mut [0u8]);
    std::process::exit(1);
}
