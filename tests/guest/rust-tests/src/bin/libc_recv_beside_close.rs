//! A libc `recv` blocked on a stream socket outlives a `close` of its
//! descriptor on another thread, as POSIX lets it, and its process lives on:
//! the socket goes once the `recv` returns, in the copy of `toyos` that
//! holds its end: libc's, in a Rust program, whose descriptor table is std's.
//!
//! It is red where libc lets the descriptor go before the call is done with
//! the stream's handle: the `close` then closes that handle under the reader,
//! whose next syscall on it ends the whole process with a handle fault.
//!
//! **This test is netstack for its child**, as `stream_handles` is: the
//! child's `connect` reaches a port of this test's, which answers with the
//! region judged and two bells, so no NIC and no peer is needed. The order is
//! awaited, never timed: the test fills the receive bell until the
//! kernel refuses another byte, and a byte it then writes lands only once the
//! child's `recv` has drained the doorbell, which is past the descriptor's
//! lookup. Only then does the child close the descriptor, and only once the
//! child said it closed does the test publish the bytes the `recv` answers.
//! It runs in a guest because the claim is the kernel's handle table under
//! libc.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use toyos::net::stream::{Server, Words, LAYOUT, RECEIVE_RING, REGION_BYTES, SERVER_WATCH};
use toyos::poller::Poller;
use toyos::net::{MsgType, RespType, TcpConnectRequest, TcpConnectResponse};
use toyos::shm::SharedMemory;
use toyos::{namespace, port, AsHandle, OwnedHandle};
use std::os::toyos::process::CommandExt;
use toyos_abi::syscall::{self, SyscallError, SVC_LABEL};

const SELF_PATH: &str = "/system/bin/test_rs_libc_recv_beside_close";

/// What libc's connect resolves.
const SERVICE: &str = "netstack";

/// The failure ceiling of a wait for the child's hang-up: a liveness bound,
/// not a timing.
const CEILING_NANOS: u64 = 20_000_000_000;

/// What the peer sends once the descriptor is closed.
const PEER: &[u8] = b"after the close";

// libc's sockets, which a Rust program links only where it asks for them.
#[link(name = "toyos_c", kind = "static")]
extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn connect(fd: i32, addr: *const u8, len: u32) -> i32;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn close(fd: i32) -> i32;
    fn __errno_location() -> *mut i32;
}

const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("child") => child(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    let (acceptor, connector) = port::create().expect("a port");
    let ns = namespace::build().add(SERVICE, &connector).finish().expect("a namespace");
    let mut child = Command::new(SELF_PATH)
        .arg("child")
        .endow(SVC_LABEL, ns.into_raw().0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    let mut tell = child.stdin.take().expect("the child's stdin");
    let mut out = BufReader::new(child.stdout.take().expect("the child's stdout"));

    let conn = acceptor.accept().expect("the child's connect");
    let header = conn.recv_header().expect("a request");
    assert_eq!(header.msg_type, MsgType::TcpConnect as u32, "a stream's connect");
    let _: TcpConnectRequest = conn.recv_payload(&header).expect("the connect's request");
    let [region] = conn.recv_handles_exact::<1>(&header).expect("the stream's region");
    let region = SharedMemory::lent(region, REGION_BYTES).expect("a region netstack takes");
    // SAFETY: the mapping is `REGION_BYTES`, 2 MiB aligned, and its header is reached only as atomics.
    let words = unsafe { &*(region.as_ptr() as *const Words) };
    let mut server = Server::new(words, LAYOUT);
    let (receive_bell, receive) = toyos::pipe_pair().expect("the receive bell");
    let (send_bell, _send) = toyos::pipe_pair().expect("the send bell");
    let bells = [OwnedHandle::from(receive_bell), OwnedHandle::from(send_bell)];
    conn.send_with(RespType::Result as u32, &TcpConnectResponse { local_port: 49_152, _pad: 0 }, bells)
        .expect("answer the connect with the bells");
    assert_eq!(line(&mut out), "connected");

    // The receive bell full, with nobody reading it yet: a write of up to
    // `MAX_WHOLE_PIPE_WRITE` lands whole or not at all, so the last of the
    // room is filled a byte at a time.
    let junk = [0u8; toyos_abi::syscall::MAX_WHOLE_PIPE_WRITE];
    for chunk in [&junk[..], &junk[..1]] {
        loop {
            match syscall::write_nonblock(receive.as_handle(), chunk) {
                Ok(_) => {}
                Err(SyscallError::WouldBlock) => break,
                Err(why) => panic!("filling the doorbell: {why:?}"),
            }
        }
    }
    writeln!(tell, "receive").expect("tell the child to receive");
    // Lands only once the child's `recv` drained the bell.
    syscall::write(receive.as_handle(), &[1]).expect("a ring past the child's drain");
    writeln!(tell, "close").expect("tell the child to close");
    let said = line(&mut out);
    if said != "closed" {
        let status = child.wait().expect("wait the child");
        panic!("the child ended {status:?} at the close of its descriptor, under its recv, saying {said:?}");
    }

    // SAFETY: the receive ring is inside the mapping, and the client reads nothing past the tail.
    unsafe { std::ptr::copy_nonoverlapping(PEER.as_ptr(), region.as_ptr().add(RECEIVE_RING), PEER.len()) };
    let _ = server.publish(words, PEER.len() as u32).expect("the client keeps the protocol");
    // A child that ended at the close holds no reader to ring.
    let _ = syscall::write(receive.as_handle(), &[1]);

    let said = line(&mut out);
    if said != format!("received {}", String::from_utf8_lossy(PEER)) {
        let status = child.wait().expect("wait the child");
        panic!("the child ended {status:?} after the close, having said {said:?}");
    }
    // The recv returned, and with it the socket's last hold: libc's copy of
    // `toyos`, which holds the stream's end, lets it go, so its connection
    // hangs up while the child lives.
    let poller = Poller::new(1);
    poller.watch_raw(conn.as_handle(), SERVER_WATCH, 0);
    let mut answered = false;
    poller.wait(1, CEILING_NANOS, |_| answered = true);
    let mut bytes = [0u8; 64];
    while syscall::read_nonblock(conn.as_handle(), &mut bytes).is_ok_and(|n| n > 0) {}
    let hung_up = answered && syscall::read_nonblock(conn.as_handle(), &mut bytes) == Ok(0);
    writeln!(tell, "exit").expect("tell the child to exit");
    let status = child.wait().expect("wait the child");
    assert!(hung_up, "the stream's end outlived its descriptor's close and its last call, in a child that lives on");
    assert!(status.success(), "the child ended {status:?}");
    println!("libc_recv_beside_close: a recv blocked across its descriptor's close answered the peer's bytes, its process lived on, and the stream went with its last call");
}

fn line(out: &mut impl BufRead) -> String {
    let mut line = String::new();
    out.read_line(&mut line).expect("a line from the child");
    line.trim().to_string()
}

fn say(what: &str) {
    println!("{what}");
    std::io::stdout().flush().expect("flush");
}

/// Connects through libc, receives on one thread and closes on this one.
fn child() {
    // SAFETY: libc's calls, each with a live buffer of the length it is given.
    let fd = unsafe { socket(AF_INET, SOCK_STREAM, 0) };
    assert!(fd >= 0, "socket answered {fd}");
    // `sockaddr_in`: the family, the port big-endian, the address, eight zeros.
    let mut addr = [0u8; 16];
    addr[..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
    addr[2..4].copy_from_slice(&80u16.to_be_bytes());
    addr[4..8].copy_from_slice(&[192, 0, 2, 2]);
    // SAFETY: as above.
    assert_eq!(unsafe { connect(fd, addr.as_ptr(), addr.len() as u32) }, 0, "connect");
    say("connected");

    let mut orders = std::io::stdin().lock();
    assert_eq!(line(&mut orders), "receive");
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        // SAFETY: as above.
        let n = unsafe { recv(fd, buf.as_mut_ptr(), buf.len(), 0) };
        // SAFETY: libc's errno of this thread, read once its call returned.
        assert!(n > 0, "recv answered {n}, errno {}", unsafe { *__errno_location() });
        say(&format!("received {}", String::from_utf8_lossy(&buf[..n as usize])));
    });
    assert_eq!(line(&mut orders), "close");
    // SAFETY: a descriptor this process opened, closed once.
    assert_eq!(unsafe { close(fd) }, 0, "close");
    say("closed");
    reader.join().expect("the reader");
    assert_eq!(line(&mut orders), "exit");
}
