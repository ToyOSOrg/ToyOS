//! One session holding all a file server lets it hold, through every program
//! launched in it, leaves the server answering another session.
//!
//! This job holds test-runner's grants and launcher, in the machine's session.
//! On DATA's server:
//!
//! - each of DATA's directories is its grant's root: a file made under
//!   `/home` is under none of the others;
//! - it takes every stream the server will give its session, until one is
//!   refused; a shell it launches, in this session, then has its redirect's
//!   stream refused;
//! - it takes every connection the server will serve its session, until one
//!   is refused; a shell it launches then has its redirect's connection
//!   refused;
//! - holding all that, a shell it launches launches another, whose row opens
//!   a login session: that shell connects to `/home` and streams a child's
//!   output into a file, and the bytes are read back here;
//! - last, it opens more connections than one session may have waiting on
//!   their hello: the ones past its share are answered `ResourceExhausted`
//!   and let go as the server takes them, so the first to end is not the
//!   first opened, which the server would otherwise let go first, at its
//!   handshake timeout; a hello on it, which finds the server gone, reads
//!   why, and every later one keeps none of the windows it could not lend.
//!   Last, because the server reaps the ones this job drops only when it
//!   next reads them, and until then they are this session's share.

use std::fs;
use std::process::{Command, Output};

use toyos::endow;
use toyos::fs::{hello, Dir, O_CREATE, O_WRITE, WINDOW_BYTES};
use toyos::ipc::Connection;
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos_abi::syscall::SyscallError;
use toyos_abi::RawHandle;

const DIR: &str = "/home/fs_share";
const OTHER: &str = "/home/fs_share/other";
const SAME_STREAM: &str = "/home/fs_share/same_stream";
const SAME_CONNECTION: &str = "/home/fs_share/same_connection";
const MARK: &str = "/home/fs_share/mark";
const SAID: &str = "answered";

/// More connections than one session's share of a server's handshakes: the
/// server's own machine-wide bound on them.
const UNANSWERED: usize = 32;

/// Far past any bound a server keeps, so a server that refuses nothing ends the
/// loop rather than the machine.
const CEILING: usize = 1024;

/// How long the first unanswered connection may take to end: a hang ceiling,
/// never a measure.
const HANG_NS: u64 = 60_000_000_000;

fn main() {
    let names = endow::namespace().expect("this job was endowed a namespace");
    // std's own connection to /home, made before anything is held, is what
    // the shell's file is read back through.
    fs::create_dir_all(DIR).expect("make the test's directory");
    for file in [OTHER, SAME_STREAM, SAME_CONNECTION] {
        let _ = fs::remove_file(file);
    }
    // Every arm runs, so one run says each one that is red.
    let mut red = Vec::new();

    fs::write(MARK, b"home's").expect("write a file under /home");
    for elsewhere in ["/apps", "/config", "/state"] {
        let path = format!("{elsewhere}/fs_share/mark");
        match fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            other => red.push(format!("{MARK} is also {path}: {other:?}")),
        }
    }

    // Every stream the server gives this session.
    let mut dir = Dir::connect(names, "fs:/home").expect("a client of /home");
    let file = dir.open("fs_share/streams", O_WRITE | O_CREATE).expect("open a file to stream into");
    let mut streams = Vec::new();
    let refused = loop {
        if streams.len() == CEILING {
            break None;
        }
        match dir.stream(file.fid, file.generation, 0) {
            Ok(pipe) => streams.push(pipe),
            Err(e) => break Some(e),
        }
    };
    match refused {
        Some(SyscallError::ResourceExhausted) => println!("  this session holds {} streams", streams.len()),
        other => red.push(format!("after {} streams, the next was answered {other:?}", streams.len())),
    }
    // A launch in this session: its redirect connects and opens the file, and
    // the stream into it is refused, so it stays empty.
    let shell = run_shell(&format!("/system/bin/toybox echo {SAID} > {SAME_STREAM}"));
    match fs::read_to_string(SAME_STREAM) {
        Ok(text) if text.is_empty() => println!("  a shell launched in this session was refused a stream"),
        other => red.push(format!("a shell launched in this session streamed {other:?}: {shell:?}")),
    }

    // Every connection the server serves this session, all lent one window.
    let window = SharedMemory::create(WINDOW_BYTES).expect("a window");
    let mut served = Vec::new();
    let refused = loop {
        if served.len() == CEILING {
            break None;
        }
        let conn = names.open("fs:/home").expect("connect to /home");
        match hello(&conn, &window) {
            Ok(_) => served.push(conn),
            Err(e) => break Some(e),
        }
    };
    match refused {
        Some(SyscallError::ResourceExhausted) => {
            println!("  this session is served {} more connections", served.len())
        }
        other => red.push(format!("after {} connections, the next hello was answered {other:?}", served.len())),
    }
    // A launch in this session: its redirect's connection is refused, so the
    // shell says so and its child echoes to the shell's own output instead.
    let shell = run_shell(&format!("/system/bin/toybox echo {SAID} > {SAME_CONNECTION}"));
    let echoed = String::from_utf8_lossy(&shell.stdout).trim_end() == SAID;
    match fs::metadata(SAME_CONNECTION) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && echoed => {
            println!("  a shell launched in this session was refused a connection")
        }
        other => red.push(format!("a shell launched in this session made {SAME_CONNECTION}: {other:?}, {shell:?}")),
    }

    // Another session, while this one holds all that: the shell this job
    // launches is in this session, and the one it launches opens a session.
    let shell = run_shell(&format!("/system/bin/shell -c '/system/bin/toybox echo {SAID} > {OTHER}'"));
    if !shell.status.success() {
        red.push(format!("the other session's shells failed: {shell:?}"));
    }
    match fs::read_to_string(OTHER) {
        Ok(text) if text.trim_end() == SAID => println!("  another session connected and streamed"),
        other => red.push(format!("another session's stream wrote {other:?}")),
    }

    // Connections that never say hello.
    let opened: Vec<Connection> =
        (0..UNANSWERED).map(|_| names.open("fs:/home").expect("connect to /home")).collect();
    let poller = Poller::new(UNANSWERED as u32);
    for (i, conn) in opened.iter().enumerate() {
        poller.watch(conn, READABLE, i as u64);
    }
    let mut ended = Vec::new();
    // Two, so the server had let the first of them go whole before the second
    // was answered: it takes one connection at a time.
    poller.wait(2, HANG_NS, |token| ended.push(token as usize));
    ended.sort_unstable();
    match ended.first() {
        None => red.push(format!("none of {UNANSWERED} unanswered connections ended")),
        Some(0) => red.push(format!("the first unanswered connection ended first, with {ended:?}")),
        Some(_) => println!("  unanswered connections past the share ended first: {ended:?}"),
    }
    // So this hello finds the server gone, and reads what it was answered.
    if let Some(&first) = ended.first() {
        match hello(&opened[first], &window) {
            Err(SyscallError::ResourceExhausted) => println!("  a hello on a connection let go is told why"),
            other => red.push(format!("a hello on connection {first}, let go, was answered {:?}", other.map(|_| ()))),
        }
        // As many hellos as this process has handle slots: one that kept the
        // window it could not lend would fill the table before the last.
        let refused = (0..RawHandle::MAX_SLOTS)
            .map(|_| hello(&opened[first], &window).map(|_| ()))
            .enumerate()
            .find(|(_, answer)| *answer != Err(SyscallError::Gone));
        match refused {
            None => println!("  {} hellos on a connection let go kept no window", RawHandle::MAX_SLOTS),
            Some((i, answer)) => red.push(format!("hello {i} on connection {first}, let go, was answered {answer:?}")),
        }
    }

    // A table the last arm filled has no room left to report it in.
    drop((poller, opened, served, streams));
    assert!(red.is_empty(), "fs_share:\n  {}", red.join("\n  "));
    println!("fs_share: PASS");
}

/// A shell launched through test-runner's launcher, in this job's session,
/// running `line`, from `/`: no file server's directory, so the redirect is
/// the only file it opens.
fn run_shell(line: &str) -> Output {
    Command::new("/system/bin/shell").args(["-c", line]).current_dir("/").output().expect("launch a shell")
}
