//! A file server's bounds are shared out by share: one holding all it may
//! leaves the server answering another, and no launch made in a login session
//! gives that session a second.
//!
//! This job holds test-runner's grants and launcher: test-runner's share, in
//! the machine's session. On DATA's server:
//!
//! - each of DATA's directories is its grant's root: a file made under
//!   `/home` is under none of the others;
//! - it takes every stream the server will give its share, until one is
//!   refused; a shell it launches, which opens no session, then has its
//!   redirect's stream refused;
//! - it takes every connection the server will serve its share, until one is
//!   refused; a shell it launches then has its redirect's connection refused;
//! - holding all that, a shell it launches launches another, which that
//!   shell's `login` row opens a login session for. Down a chain of shells
//!   each launching the next, every one in that session, each streams a
//!   child's output into a file and holds one more stream, its next shell's
//!   output. The first's file shows another session's first connection and
//!   first stream answered; every one's up to the session's share of streams
//!   is written, and the next shell's is refused: a `login` row's launches in
//!   a login session opened none;
//! - last, it opens more connections than one share may have waiting on
//!   their hello: the ones past it are answered `ResourceExhausted`
//!   and let go as the server takes them, so the first to end is not the
//!   first opened, which the server would otherwise let go first, at its
//!   handshake timeout; a hello on it, which finds the server gone, reads
//!   why, and every later one keeps none of the windows it could not lend.
//!   Last, because the server reaps the ones this job drops only when it
//!   next reads them, and until then they are this share's.

use std::fs;
use std::process::Command;

use toyos::endow;
use toyos::fs::{hello, Dir, O_CREATE, O_WRITE, WINDOW_BYTES};
use toyos::ipc::Connection;
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos_abi::syscall::SyscallError;
use toyos_abi::RawHandle;

const DIR: &str = "/home/fs_share";
const SAME_STREAM: &str = "/home/fs_share/same_stream";
const SAME_CONNECTION: &str = "/home/fs_share/same_connection";
const FIRST: &str = "/home/fs_share/first";
const PAST: &str = "/home/fs_share/past";
const MARK: &str = "/home/fs_share/mark";
const SAID: &str = "answered";

/// More connections than one share of a server's handshakes: the server's own
/// machine-wide bound on them.
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
    for file in [SAME_STREAM, SAME_CONNECTION, FIRST, PAST] {
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

    // Every stream the server gives this share.
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
        Some(SyscallError::ResourceExhausted) => println!("  this share holds {} streams", streams.len()),
        other => red.push(format!("after {} streams, the next was answered {other:?}", streams.len())),
    }
    // A launch that opens no session: its redirect connects and opens the
    // file, and the stream into it is refused, so it stays empty.
    let shell = run_shell(&format!("/system/bin/toybox echo {SAID} > {SAME_STREAM}")).output().expect("launch a shell");
    match fs::read_to_string(SAME_STREAM) {
        Ok(text) if text.is_empty() => println!("  a shell launched under this share was refused a stream"),
        other => red.push(format!("a shell launched under this share streamed {other:?}: {shell:?}")),
    }

    // Every connection the server serves this share, all lent one window.
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
            println!("  this share is served {} more connections", served.len())
        }
        other => red.push(format!("after {} connections, the next hello was answered {other:?}", served.len())),
    }
    // A launch that opens no session: its redirect's connection is refused,
    // so the file is never made.
    let shell = run_shell(&format!("/system/bin/toybox echo {SAID} > {SAME_CONNECTION}")).output().expect("launch a shell");
    match fs::metadata(SAME_CONNECTION) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("  a shell launched under this share was refused a connection")
        }
        other => red.push(format!("a shell launched under this share made {SAME_CONNECTION}: {other:?}, {shell:?}")),
    }

    // A login session, while this share holds all that: the shell this job
    // launches opens none, and the one it launches is in one its row opens.
    // The first shell in it streams, and every shell holds its next one's
    // output, one stream more, until the session holds as many as this share
    // does; the last asks for one past that. Each link's line is a variable
    // the next shell expands, so no line is quoted inside another.
    let links = streams.len() + 1;
    let mut chain = run_shell("/system/bin/shell -c $FS_SHARE_1");
    for link in 1..=links {
        let next = format!("/system/bin/shell -c $FS_SHARE_{} > {DIR}/next_{link}", link + 1);
        let line = match link {
            1 => format!("/system/bin/toybox echo {SAID} > {FIRST} ; {next}"),
            _ if link == links => format!("/system/bin/toybox echo {SAID} > {PAST}"),
            _ => next,
        };
        chain.env(format!("FS_SHARE_{link}"), line);
    }
    let chain = chain.output();
    match fs::read_to_string(FIRST) {
        Ok(text) if text.trim_end() == SAID => println!("  a login session connected and streamed"),
        other => red.push(format!("a login session's first stream wrote {other:?}: {chain:?}")),
    }
    match fs::read_to_string(PAST) {
        Ok(text) if text.is_empty() => {
            println!("  a login session's launches held {} streams and the next was refused", links - 1)
        }
        other => red.push(format!("past {} streams, a login session's launch streamed {other:?}: {chain:?}", links - 1)),
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

/// A shell launched through test-runner's launcher, under this job's share,
/// running `line`, from `/`: no file server's directory, so a redirect is the
/// only file it opens.
fn run_shell(line: &str) -> Command {
    let mut shell = Command::new("/system/bin/shell");
    shell.args(["-c", line]).current_dir("/");
    shell
}
