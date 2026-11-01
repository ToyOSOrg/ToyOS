//! With one ordinary job holding every frame it may, the machine's controls
//! still work, and what the exhaustion stopped starts again once the memory
//! is given back.
//!
//! This job runs in test-runner's, which `tests/images/floorcase.toml` gives a
//! floor: it stands in for the stop UI, a system holder. It starts a greedy
//! job of its own, an ordinary one, which commits every frame it may; then,
//! while that holds them:
//!
//! - a server in an ordinary job answers this job's connection, before any of
//!   that, and its account does not move: a connection's two directions are
//!   its client's;
//! - an ordinary job's spawn is refused, so the exhaustion is real;
//! - the supervisor answers a stop's request on a new connection, which takes
//!   nothing of the supervisor's own;
//! - `starveling`, an ordinary service, is asked to take memory and ends,
//!   refused it; the supervisor is refused its restart, which it says in the
//!   log, and keeps its port, where a new client's request waits;
//! - with `--input`, a key the host types reaches `inputsink` through
//!   ps2server, whose floor holds the frames its connection's writes take;
//!
//! and then it kills the greedy job, and the waiting request is answered by a
//! `starveling` the supervisor started again because memory came back.
//!
//! It needs the guest: what it holds is the kernel's accounting across jobs,
//! the supervisor's restart and the input path together, which no host
//! model has. It holds every frame an ordinary job may, so it has a boot of
//! its own: `memory_floor_holds_the_controls` on QEMU q35, and the
//! `memory_floor` metal row on the T14, without `--input`.

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow::{self, Endowments, SYSCAP_LABEL};
use toyos::ipc::Connection;
use toyos::job::JOB_LABEL;
use toyos::poller::{Poller, READABLE};
use toyos::power::{self, Refused};
use toyos::syscap::SysCap;
use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt};

const ME: &str = "/system/bin/test_rs_memory_floor";
const PAGE: usize = 4096;

/// `starveling`'s frames.
const PING: u32 = 1;
const SERVED: u32 = 2;
const STARVE: u32 = 3;
const RELEASED: u32 = 4;
const END: u32 = 5;

/// `inputsink`'s frames on `keys`.
const HELLO: u32 = 1;
const KEY: u32 = 2;

/// What the supervisor says when its restart of `starveling` is refused for
/// memory, after the pid (`system/supervisor`, `Supervisor::ended`).
const REFUSED_RESTART: &str = "ended and could not start again for want of memory";

/// How long each event is waited for before the test calls it lost: a hang
/// ceiling, never a timing claim.
const CEILING: Duration = Duration::from_secs(120);

/// What the host waits for before it types: the job holds every frame.
const PRESS: &str = "memory_floor: every frame is held; press a key";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("greedy") => greedy(),
        Some("idle") => {}
        Some("serve") => serve(),
        Some("--input") => control(true),
        None => control(false),
        Some(other) => panic!("memory_floor: no arm {other}"),
    }
}

fn control(input: bool) {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows the estate's capability");
    let own = toyos::job::own().expect("a handle to this job");
    let account = own.account().expect("this job's account");
    assert!(account.floor > 0, "this job has no floor: the image gives test-runner one");
    println!("  memory_floor: this job's floor is {} KiB, and it holds {} KiB", account.floor >> 10, (account.committed + account.kernel) >> 10);

    // Every connection that crosses the exhaustion is made and used once
    // before it, so each of its pipes keeps a frame for what crosses it then.
    let keys = input.then(|| {
        let keys = endow::service("keys").expect("the image gives test-runner `keys`");
        keys.signal(HELLO).expect("hello to inputsink");
        assert_eq!(frame(&keys).0, HELLO, "inputsink's answer to hello");
        keys
    });
    let starved = endow::service("starved").expect("the image gives test-runner `starved`");
    ping(&starved).expect("starveling served before anything is taken");

    // The log from its first line, which logkeeper, on this image's floor,
    // goes on handing over: where the supervisor's refusal is read.
    let mut log = Log { pipe: logkeeper_api::read().expect("the log, before anything is taken").pipe, bytes: vec![0; 64 << 10], held: 0 };
    log.drain();

    directions_are_the_clients(&cap);

    let job = cap.new_job().expect("an ordinary job for the greedy one");
    let mut greedy = Command::new(ME)
        .arg("greedy")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .endow(JOB_LABEL, job.duplicate().expect("a second handle to its job").into())
        .spawn()
        .expect("spawn the greedy job");
    let mut said = BufReader::new(greedy.stdout.take().expect("its stdout"));
    let mut line = String::with_capacity(256);
    for want in ["greedy: taking", "greedy: holding"] {
        line.clear();
        said.read_line(&mut line).expect("a line of the greedy job's");
        assert!(line.starts_with(want), "the greedy job said {line:?}, and was to say {want:?}");
    }
    print!("  {line}");
    let held = job.account().expect("the greedy job's account");
    println!("  memory_floor: the greedy job holds {} MiB committed", held.committed >> 20);

    // An ordinary job is refused: the exhaustion is the machine's.
    let other = cap.new_job().expect("a job is a few bytes this job's floor holds");
    let refused = Command::new(ME)
        .arg("idle")
        .endow(JOB_LABEL, other.into())
        .spawn()
        .map(|_| ());
    assert!(
        refused.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::OutOfMemory),
        "an ordinary job's spawn with every frame held answered {refused:?}"
    );
    println!("  PASS: an ordinary job's spawn is refused for memory");

    // The stop's request, on a connection the supervisor accepts now.
    assert_eq!(power::cancel(), Err(Refused::NoStopPending), "the supervisor's answer to a stop's cancel");
    println!("  PASS: the supervisor answers a stop's request");

    // The restart: starveling is refused memory, gives back most of its
    // stack, and ends. The greedy job takes what came back, so starveling's
    // end frees less than another start of it needs, and the restart is
    // refused.
    let before = job.account().expect("the greedy job's account").committed;
    starved.signal(STARVE).expect("ask starveling to take memory");
    let (msg_type, payload) = frame(&starved);
    assert_eq!(msg_type, RELEASED, "starveling's answer to being refused memory");
    let given = u64::from_le_bytes(payload[..8].try_into().expect("how much it gave back"));
    // Each line the greedy job says is a take. It has taken what came back
    // once, after a take since starveling was asked, its job is not ready:
    // what it was last refused is not free for it.
    let taken = loop {
        line.clear();
        said.read_line(&mut line).expect("a line of the greedy job's");
        assert!(line.starts_with("greedy: took "), "the greedy job said {line:?}");
        let taken = job.account().expect("the greedy job's account").committed.saturating_sub(before);
        if taken > 0 && !ready_now(&job) {
            break taken;
        }
    };
    println!("  memory_floor: the greedy job took back {} KiB, starveling having given back {} KiB", taken >> 10, given >> 10);
    starved.signal(END).expect("tell starveling to end");
    assert!(toyos::ipc::FrameRx::<0>::new().recv(&starved).is_err(), "starveling answered, and was to end");
    log.await_line(&["supervisor: starveling (pid ", REFUSED_RESTART]);
    println!("  PASS: starveling ended, and its restart was refused for memory");
    // Asked of a port no process serves now: it waits for the next.
    let waiting = endow::service("starved").expect("a connection waits on starveling's kept port");
    waiting.signal(PING).expect("a request to the starveling to come");

    if let Some(keys) = &keys {
        println!("{PRESS}");
        await_readable(keys, "key relayed by inputsink");
        let (msg_type, payload) = frame(keys);
        assert_eq!(msg_type, KEY, "inputsink relayed {msg_type}, not a key");
        println!("  PASS: key {:#04x} reached the owner of input through ps2server", payload[0]);
    }

    greedy.kill().expect("the system holder kills the greedy job");
    greedy.wait().expect("the greedy job's end");
    println!("  PASS: the greedy job is killed");

    await_readable(&waiting, "starveling started again to answer");
    assert_eq!(frame(&waiting).0, SERVED, "the restarted starveling's answer");
    println!("  PASS: starveling serves again once the memory is given back");
    println!("memory_floor: the controls worked with every frame held, and the starved service came back");
}

/// A server in an ordinary job of its own accepts this job's connection and
/// answers it: both of the connection's directions are its client's, so the
/// server's reply costs its job nothing, and no client can spend a server's
/// floor by connecting to it.
fn directions_are_the_clients(cap: &SysCap) {
    let (acceptor, connector) = toyos::port::create().expect("a port");
    let job = cap.new_job().expect("a job for the server");
    let mut server = Command::new(ME)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .endow(JOB_LABEL, job.duplicate().expect("a second handle to its job").into())
        .endow("acceptor", acceptor.into())
        .spawn()
        .expect("spawn the server");
    let names = toyos::namespace::build().add("served", &connector).finish().expect("a namespace of the port");
    let conn = names.open("served").expect("connect to the server");
    let mut said = BufReader::new(server.stdout.take().expect("its stdout"));
    let mut line = String::new();
    said.read_line(&mut line).expect("the server's line");
    assert_eq!(line.trim(), "serve: accepted");
    let before = job.account().expect("the server's account");
    ping(&conn).expect("the server answers");
    line.clear();
    said.read_line(&mut line).expect("the server's line");
    assert_eq!(line.trim(), "serve: answered");
    let after = job.account().expect("the server's account");
    assert_eq!(after.kernel, before.kernel, "the server's reply was charged to the server");
    server.kill().expect("end the server");
    server.wait().expect("the server's end");
    println!("  PASS: a connection's directions are its client's: the server's reply cost its job nothing");
}

/// Accept one connection on the endowed acceptor and answer its [`PING`].
fn serve() -> ! {
    let acceptor: toyos::port::Acceptor = Endowments::get().take("acceptor").expect("serve: an acceptor");
    let poller = Poller::new(1);
    poller.watch(&acceptor, READABLE, 0);
    poller.wait(1, u64::MAX, |_| {});
    let conn = acceptor.accept().expect("serve: the connection");
    println!("serve: accepted");
    assert_eq!(frame(&conn).0, PING, "serve: the request");
    conn.signal(SERVED).expect("serve: the answer");
    println!("serve: answered");
    let mut stdin = [0u8; 1];
    loop {
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut stdin);
    }
}

/// Commit every page a promise can still be made for, and take again what
/// comes back, each time its job is told memory it was refused is free, until
/// it is killed.
fn greedy() -> ! {
    println!("greedy: taking every frame");
    // Before anything is taken: a poller is an inbox's ring of memory.
    let job = toyos::job::own().expect("greedy: a handle to its own job");
    let poller = Poller::new(1);
    println!("greedy: holding {} MiB, refused a page more", take() >> 20);
    loop {
        poller.watch(&job, READABLE, 0);
        poller.wait(1, u64::MAX, |_| {});
        let took = take();
        if took > 0 {
            println!("greedy: took {} KiB more", took >> 10);
        }
    }
}

/// Every page a promise can still be made for, committed and never released:
/// how many bytes.
fn take() -> usize {
    let mut committed = 0usize;
    let mut len = 1usize << 40;
    while len >= PAGE {
        // SAFETY: placed anywhere, so it replaces nothing; never released.
        match unsafe { syscall::vm_reserve(len, VmProt::ReadWrite, VmAt::ANYWHERE) } {
            Ok(_) => committed += len,
            Err(SyscallError::ResourceExhausted | SyscallError::InvalidArgument) => len /= 2,
            Err(e) => panic!("greedy: a commitment of {len} bytes answered {e:?}"),
        }
    }
    committed
}

/// Whether `job` is readable now: what it was last refused is free for it.
fn ready_now(job: &toyos::job::Job) -> bool {
    let poller = Poller::new(1);
    poller.watch(job, READABLE, 0);
    let mut woke = false;
    poller.wait(1, 0, |_| woke = true);
    woke
}

/// [`PING`] answered [`SERVED`].
fn ping(conn: &Connection) -> Result<(), String> {
    conn.signal(PING).map_err(|e| format!("{e:?}"))?;
    match frame(conn).0 {
        SERVED => Ok(()),
        other => Err(format!("answered {other}")),
    }
}

/// One whole frame off `conn`, waiting for it.
fn frame(conn: &Connection) -> (u32, Vec<u8>) {
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("a frame");
    (frame.msg_type, rx.payload(frame.payload_len).to_vec())
}

/// Wait until `conn` has something to read, within [`CEILING`].
fn await_readable(conn: &Connection, what: &str) {
    let poller = Poller::new(1);
    poller.watch(conn, READABLE, 0);
    let mut woke = false;
    poller.wait(1, CEILING.as_nanos() as u64, |_| woke = true);
    assert!(woke, "{} s passed with no {what}", CEILING.as_secs());
}

/// The log as logkeeper hands it over, read into a buffer made before
/// anything is taken.
struct Log {
    pipe: toyos::Pipe,
    bytes: Vec<u8>,
    /// Bytes of an unfinished line kept at the buffer's start.
    held: usize,
}

impl Log {
    /// Read what has been handed over so far, keeping none of it.
    fn drain(&mut self) {
        while self.ready(Duration::ZERO) {
            let n = self.pipe.read(&mut self.bytes).expect("read the log");
            assert!(n > 0, "logkeeper let the log go");
        }
    }

    /// Whether a read would find bytes within `bound`.
    fn ready(&self, bound: Duration) -> bool {
        let poller = Poller::new(1);
        poller.watch(&self.pipe, READABLE, 0);
        let mut woke = false;
        poller.wait(1, bound.as_nanos() as u64, |_| woke = true);
        woke
    }

    /// Wait, within [`CEILING`], for a line holding every one of `parts`.
    fn await_line(&mut self, parts: &[&str]) {
        let until = Instant::now() + CEILING;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            assert!(self.ready(left), "{} s passed with no line of {parts:?} in the log", CEILING.as_secs());
            let n = self.pipe.read(&mut self.bytes[self.held..]).expect("read the log");
            assert!(n > 0, "logkeeper let the log go");
            let end = self.held + n;
            let last = self.bytes[..end].iter().rposition(|&b| b == b'\n').map_or(0, |at| at + 1);
            let whole = String::from_utf8_lossy(&self.bytes[..last]);
            if whole.lines().any(|line| parts.iter().all(|part| line.contains(part))) {
                return;
            }
            self.bytes.copy_within(last..end, 0);
            self.held = end - last;
            assert!(self.held < self.bytes.len(), "a line of the log longer than {} bytes", self.bytes.len());
        }
    }
}
