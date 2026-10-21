//! A stream belongs to the process that connected or accepted it, and std
//! keeps it so on every path a raw handle takes.
//!
//! - **Taken back, a handle shares its one end.** A stream's handle given out
//!   by `into_raw_fd` or `From<TcpStream> for OwnedFd`, or made by
//!   `try_clone_to_owned`, and taken back reads on where the last read
//!   stopped: one end, one set of cursors.
//! - **A handle the process holds no end for is refused by name**, and nothing
//!   is written on its connection: `TcpStream::from_raw_fd` of a bare `dup`
//!   panics with `toyos::net`'s refusal.
//! - **A stream's handle is no file**: `File::from_raw_fd` of one panics with
//!   that refusal.
//! - **No spawn moves one**: a stdio descriptor and one past them by
//!   `inherit_handle`, `endow` and `provide` each answer the refusal, start no
//!   child, and leave the stream reading and writing through the handle.
//!
//! - **netstack's watch of a stream's connection** (`SERVER_WATCH`) is one
//!   the kernel takes, and is answered by the client's hang-up.
//!
//! **This test is netstack for its children**: a port of its own served to each
//! as `netstack`, as `netstack_gone_mid_bind` does, which answers a connect
//! with the region judged, [`PEER`]'s bytes published into it as netstack
//! publishes what arrived, and a bell, so no NIC and no peer is needed. It
//! runs in a guest because std's ToyOS arms build only for `toyos` and stand
//! on the kernel's handle table.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process::CommandExt;
use std::process::{Child, Command, Stdio};

use toyos::net::stream::{Misuse, Server, Words, LAYOUT, RECEIVE_RING, REGION_BYTES, SERVER_WATCH};
use toyos::poller::Poller;
use toyos::net::{MsgType, RespType, TcpConnectRequest, TcpConnectResponse};
use toyos::shm::SharedMemory;
use toyos::{namespace, port, AsHandle, Connection, OwnedHandle, Pipe};
use toyos_abi::syscall::{self, SVC_LABEL};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_stream_handles";

/// What `toyos::net`'s connect resolves.
const SERVICE: &str = "netstack";

/// Where the children connect: nothing answers there but this test.
const ADDR: &str = "192.0.2.2:80";

/// What the peer sent before the client read anything.
const PEER: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("client") => client(),
        Some("foreign") => foreign(),
        Some("as-file") => as_file(),
        Some("child") => {
            say("started");
            std::process::exit(0)
        }
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    shares_and_refuses_moves();
    refuses_a_foreign_handle_writing_nothing();
    refuses_a_stream_as_a_file();
    println!("a stream's handle reaches its one end in its own process and goes nowhere else");
}

/// The handles given out and taken back share one end, and no spawn moves one.
fn shares_and_refuses_moves() {
    let (mut child, netstack) = run("client");
    let out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    let said: Vec<String> = out.lines().map(|line| line.expect("a line")).collect();
    let status = child.wait().expect("wait the client");
    assert_eq!(
        said,
        ["shared", "stdio descriptor refused", "descriptor past stdio refused", "endow refused", "provide refused"],
        "the client said otherwise",
    );
    assert!(status.success(), "the client exited {status:?}");
    println!("  given out and taken back: one end; every spawn path refused, and the stream still serves");
    hang_up_answers_the_server_watch(&netstack.conn);
}

/// netstack's watch of a stream's connection, made after its client left: the
/// kernel takes it and answers it, and the connection then reads its rings and
/// its end.
fn hang_up_answers_the_server_watch(conn: &Connection) {
    let poller = Poller::new(1);
    poller.watch_raw(conn.as_handle(), SERVER_WATCH, 0);
    let mut answers = Vec::new();
    poller.wait_answers(1, 10_000_000_000, |_, answer| answers.push(answer));
    assert!(matches!(answers[..], [Ok(_)]), "the kernel answered netstack's watch {answers:?}");
    let mut bytes = [0u8; 64];
    while syscall::read_nonblock(conn.as_handle(), &mut bytes).is_ok_and(|n| n > 0) {}
    assert_eq!(syscall::read_nonblock(conn.as_handle(), &mut bytes), Ok(0), "the hang-up is the connection's end");
    println!("  netstack's watch of a stream's connection: taken, and answered by the hang-up");
}

/// A bare `dup` of a stream's handle is no stream of the process: refused by
/// name, with nothing written on its connection before the process ends.
fn refuses_a_foreign_handle_writing_nothing() {
    let (mut child, netstack) = run("foreign");
    let mut out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    assert_eq!(line(&mut out), "duplicated");
    // What the connect may have rung, taken; from here the connection carries nothing but its end.
    let mut bytes = [0u8; 64];
    while syscall::read_nonblock(netstack.conn.as_handle(), &mut bytes).is_ok_and(|n| n > 0) {}
    child.stdin.take().expect("the child's stdin").write_all(b"go\n").expect("tell the child");
    let said = child.wait_with_output().expect("wait the child");
    let stderr = String::from_utf8_lossy(&said.stderr);
    assert!(!said.status.success(), "the foreign handle was taken: {}", String::from_utf8_lossy(&said.stdout));
    assert!(
        stderr.contains("is no stream of this process's: a stream belongs to the process that connected or accepted it"),
        "refused, but not by name: {stderr}",
    );
    let read = syscall::read(netstack.conn.as_handle(), &mut bytes);
    assert_eq!(read, Ok(0), "the refusal wrote on the stream's connection before its end");
    println!("  a foreign handle: refused by name, nothing written");
}

/// `File::from_raw_fd` of a stream's handle panics with the refusal.
fn refuses_a_stream_as_a_file() {
    let (child, _netstack) = run("as-file");
    let said = child.wait_with_output().expect("wait the child");
    let stderr = String::from_utf8_lossy(&said.stderr);
    assert!(!said.status.success(), "the stream was taken as a file: {}", String::from_utf8_lossy(&said.stdout));
    assert!(stderr.contains("is a stream's, which is read and written through its region, not as a file"), "refused, but not by name: {stderr}");
    println!("  a stream as a file: refused by name");
}

/// The stream's server end as this test plays netstack, kept for the test's run.
struct Netstack {
    conn: Connection,
    _region: SharedMemory,
    _bell: Pipe,
    _server: Server,
}

/// Spawns `role` with this test as its netstack, and serves its connect.
fn run(role: &str) -> (Child, Netstack) {
    let (acceptor, connector) = port::create().expect("a port");
    let ns = namespace::build().add(SERVICE, &connector).finish().expect("a namespace");
    let child = Command::new(SELF_PATH)
        .arg(role)
        .endow(SVC_LABEL, ns.into_raw().0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {role}: {e}"));
    let conn = acceptor.accept().expect("the child's connect");
    let header = conn.recv_header().expect("a request");
    assert_eq!(header.msg_type, MsgType::TcpConnect as u32, "a stream's connect");
    let _: TcpConnectRequest = conn.recv_payload(&header).expect("the connect's request");
    let [region] = conn.recv_handles_exact::<1>(&header).expect("the stream's region");
    let region = SharedMemory::lent(region, REGION_BYTES).expect("a region netstack takes");
    // SAFETY: the mapping is `REGION_BYTES`, 2 MiB aligned, and its header is reached only as atomics.
    let words = unsafe { &*(region.as_ptr() as *const Words) };
    let mut server = Server::new(words, LAYOUT);
    // SAFETY: the receive ring is inside the mapping, and the client reads nothing yet.
    unsafe { std::ptr::copy_nonoverlapping(PEER.as_ptr(), region.as_ptr().add(RECEIVE_RING), PEER.len()) };
    let _ = server.publish(words, PEER.len() as u32).expect("the client keeps the protocol");
    let (bell, ring) = toyos::pipe_pair().expect("the bell");
    conn.send_with(RespType::Result as u32, &TcpConnectResponse { local_port: 49_152, _pad: 0 }, [OwnedHandle::from(bell)])
        .expect("answer the connect with the bell");
    (child, Netstack { conn, _region: region, _bell: ring, _server: server })
}

fn line(out: &mut impl BufRead) -> String {
    let mut line = String::new();
    out.read_line(&mut line).expect("a line from a child");
    line.trim().to_string()
}

fn say(what: &str) {
    println!("{what}");
    std::io::stdout().flush().expect("flush");
}

fn connect() -> TcpStream {
    TcpStream::connect(ADDR.parse::<SocketAddr>().expect("an address")).expect("this test's netstack answers")
}

/// The next `n` bytes the stream reads, which must be the peer's from `at`.
fn reads(stream: &mut TcpStream, at: usize, n: usize) {
    let mut got = vec![0u8; n];
    stream.read_exact(&mut got).expect("the peer's bytes");
    assert_eq!(got, &PEER[at..at + n], "read from where the last read stopped");
}

/// A client whose stream's handle is given out three ways and taken back, then
/// handed to every spawn path.
fn client() -> ! {
    let stream = connect();
    let raw = stream.into_raw_fd();
    // SAFETY: the handle `into_raw_fd` just gave out, taken back once.
    let mut given = unsafe { TcpStream::from_raw_fd(raw) };
    reads(&mut given, 0, 4);
    let owned: OwnedFd = given.into();
    let mut taken = TcpStream::from(owned);
    reads(&mut taken, 4, 4);
    let mut cloned = TcpStream::from(taken.as_fd().try_clone_to_owned().expect("a second handle"));
    reads(&mut cloned, 8, 4);
    drop(cloned);
    reads(&mut taken, 12, 4);
    say("shared");

    let handle = taken.as_raw_handle();
    let paths: [(&str, fn(&mut Command, u32)); 4] = [
        ("stdio descriptor", |command, handle| {
            command.inherit_handle(1, handle);
        }),
        ("descriptor past stdio", |command, handle| {
            command.inherit_handle(5, handle);
        }),
        ("endow", |command, handle| {
            command.endow("stream", handle);
        }),
        ("provide", |command, handle| {
            command.provide("stream", handle);
        }),
    ];
    let mut at = 16;
    for (path, moving) in paths {
        let mut command = Command::new(SELF_PATH);
        command.arg("child").stdout(Stdio::null());
        moving(&mut command, handle);
        match command.spawn() {
            Err(refused)
                if refused.kind() == std::io::ErrorKind::InvalidInput
                    && refused.to_string() == Misuse::Moved(RawHandle(handle)).to_string() =>
            {
                taken.write_all(b"x").unwrap_or_else(|e| panic!("{path}: the stream writes no more: {e}"));
                reads(&mut taken, at, 1);
                at += 1;
                say(&format!("{path} refused"));
            }
            Err(other) => say(&format!("{path} answered {other}")),
            Ok(mut child) => {
                let _ = child.wait();
                say(&format!("{path} took the stream's handle"));
            }
        }
    }
    std::process::exit(0)
}

/// A client that takes a bare `dup` of its stream's handle as a stream.
fn foreign() -> ! {
    let stream = connect();
    let copy = syscall::dup(RawHandle(stream.as_raw_handle())).expect("a bare duplicate");
    let fd = toyos::fd::install(toyos::fd::Object::handle(copy), false, 0).expect("a descriptor for the duplicate");
    say("duplicated");
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).expect("the test's go");
    // SAFETY: a descriptor this process holds and nothing else answers for.
    let _adopted = unsafe { TcpStream::from_raw_fd(fd) };
    say("adopted");
    std::process::exit(0)
}

/// A client that takes its stream's handle as a file.
fn as_file() -> ! {
    let stream = connect();
    // SAFETY: a handle this process holds; the panic is the claim.
    let file = unsafe { File::from_raw_fd(stream.as_raw_fd()) };
    say("taken as a file");
    let _ = file.into_raw_fd();
    std::process::exit(0)
}
