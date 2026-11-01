//! A libc `recv` blocked on a stream socket, with no signal handler kept and
//! under one, outlives a `close` of its descriptor on another thread and the
//! number's reuse, as POSIX lets it, and its process lives on: the `recv`
//! answers the stream it holds, and the socket goes once it returns, in the
//! copy of `toyos` that holds its end: libc's, in a Rust program, whose
//! descriptor table is std's.
//!
//! It is red where libc lets the descriptor go before the call is done with
//! the stream's handle: the `close` then closes that handle under the reader,
//! whose next syscall on it ends the whole process with a handle fault. It is
//! red too where the handler's interruptible wait looks the number up again:
//! the number then names a pipe nobody writes, and the reader never looks at
//! its stream again.
//!
//! **This test is netstack for its child**, as `stream_handles` is: the
//! child's `connect` reaches a port of this test's, which answers with the
//! region judged and its bells, so no NIC and no peer is needed. The order is
//! awaited, never timed: a look of the child's `recv` that finds the ring
//! empty arms the ring's want for data, which the test cleared, and that is
//! past the descriptor's lookup. Only then does the child close the
//! descriptor and give its number to a pipe; with a handler kept, it then
//! raises the signal, which the reader takes, so its wait starts again after
//! the reuse. Once the child said so, the test rings twice, each time
//! clearing the want and awaiting the reader's look: the reader may look once
//! on its way back to its wait, and only a reader whose wait watches its
//! stream looks at the second. Then the test publishes the bytes the `recv`
//! answers. It runs in a guest because the claim is the kernel's handle table
//! under libc.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

use toyos::net::stream::{Server, Words, LAYOUT, RECEIVE_RING, REGION_BYTES};
use toyos::wake::{self, Hungup};
use toyos::net::{MsgType, RespType, TcpConnectRequest, TcpConnectResponse};
use toyos::shm::SharedMemory;
use toyos::{namespace, port, OwnedHandle};
use std::os::toyos::process::CommandExt;
use toyos_abi::syscall::SVC_LABEL;

const SELF_PATH: &str = "/system/bin/test_rs_libc_recv_beside_close";

/// What libc's connect resolves.
const SERVICE: &str = "netstack";

/// The failure ceiling of a wait for the child's hang-up: a liveness bound,
/// not a timing.
const CEILING_NANOS: u64 = 20_000_000_000;

/// What the peer sends once the descriptor is closed.
const PEER: &[u8] = b"after the close";

/// The receive ring's want for data, the client's: `stream::LAYOUT`'s third
/// receive word.
const RECEIVE_WANT: usize = 2 * 16;

// libc's sockets, which a Rust program links only where it asks for them.
#[link(name = "toyos_c", kind = "static")]
extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn connect(fd: i32, addr: *const u8, len: u32) -> i32;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn close(fd: i32) -> i32;
    fn pipe(fds: *mut i32) -> i32;
    fn dup2(old: i32, new: i32) -> i32;
    fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
    fn sigprocmask(how: i32, set: *const u64, old: *mut u64) -> i32;
    fn raise(sig: i32) -> i32;
    fn read(fd: i32, buf: *mut u8, len: usize) -> isize;
    fn write(fd: i32, buf: *const u8, len: usize) -> isize;
    fn __errno_location() -> *mut i32;
}

const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;
/// A signal of libc's own, which a process may handle with no notice to
/// listen to; this child starts no child, so only its own `raise` raises it.
const SIGCHLD: i32 = 17;
const SA_RESTART: u64 = 0x1000_0000;

/// `struct sigaction` as `<signal.h>` lays it out.
#[repr(C)]
struct SigAction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

const SIG_BLOCK: i32 = 0;

/// The write end the handler says it ran on.
static RAN: AtomicI32 = AtomicI32::new(-1);

/// A handler kept, so libc's blocking calls wait interruptibly.
extern "C" fn kept(_: i32) {
    // SAFETY: `write` is async-signal-safe, of one byte from a live place.
    unsafe { write(RAN.load(Ordering::Relaxed), &1u8, 1) };
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("child") => child(false),
        Some("child-handling") => child(true),
        Some(other) => panic!("unknown role {other:?}"),
        None => {
            // A blocking `recv`, and one that waits interruptibly.
            test("child");
            test("child-handling");
            println!("libc_recv_beside_close: a recv blocked across its descriptor's close and the number's reuse, with no handler and under a kept one, answered the peer's bytes, its process lived on, and the stream went with its last call");
        }
    }
}

fn test(role: &str) {
    let (acceptor, connector) = port::create().expect("a port");
    let ns = namespace::build().add(SERVICE, &connector).finish().expect("a namespace");
    let mut child = Command::new(SELF_PATH)
        .arg(role)
        .endow(SVC_LABEL, ns.into())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    let mut tell = child.stdin.take().expect("the child's stdin");
    let mut out = BufReader::new(child.stdout.take().expect("the child's stdout"));

    let conn = acceptor.accept().expect("the child's connect");
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("a request");
    assert_eq!(frame.msg_type, MsgType::TcpConnect as u32, "a stream's connect");
    let _: TcpConnectRequest = toyos::ipc::decode_payload(rx.payload(frame.payload_len)).expect("the connect's request");
    let [region] = rx.take_handles::<1>().expect("the stream's region");
    let region = SharedMemory::lent(region, REGION_BYTES).expect("a region netstack takes");
    // SAFETY: the mapping is `REGION_BYTES`, 2 MiB aligned, and its header is reached only as atomics.
    let words = unsafe { &*(region.as_ptr() as *const Words) };
    let mut server = Server::new(words, LAYOUT);
    let (receive, receive_bell) = wake::pair().expect("the receive bell");
    let (_send, send_bell) = wake::pair().expect("the send bell");
    let (rung_waker, rung) = wake::pair().expect("the rung bell");
    let bells = [OwnedHandle::from(receive_bell), send_bell.into(), rung_waker.into()];
    conn.send_with(RespType::Result as u32, &TcpConnectResponse { local_port: 49_152, _pad: 0 }, bells)
        .expect("answer the connect with the bells");
    assert_eq!(line(&mut out), "connected");

    words[RECEIVE_WANT].store(0, Ordering::Release);
    writeln!(tell, "receive").expect("tell the child to receive");
    if !looked(words) {
        let _ = child.kill();
        panic!("the {role}'s recv never looked at its stream");
    }
    writeln!(tell, "close").expect("tell the child to close");
    let said = line(&mut out);
    if said != "closed" {
        let status = child.wait().expect("wait the child");
        panic!("the {role} ended {status:?} at the close of its descriptor, under its recv, saying {said:?}");
    }
    // The reader may look once on its way back to its wait; only a reader
    // whose wait watches the stream looks at the second ring.
    for _ in 0..2 {
        words[RECEIVE_WANT].store(0, Ordering::Release);
        receive.ring().expect("ring the reader");
        if !looked(words) {
            let _ = child.kill();
            panic!("the {role}'s recv never looked at its stream again once its descriptor's number named a pipe");
        }
    }

    region.bytes().write(RECEIVE_RING, PEER);
    let _ = server.publish(words, PEER.len() as u32).expect("the client keeps the protocol");
    receive.ring().expect("ring the reader");

    let said = line(&mut out);
    if said != format!("received {}", String::from_utf8_lossy(PEER)) {
        let status = child.wait().expect("wait the child");
        panic!("the {role} ended {status:?} after the close, having said {said:?}");
    }
    // The recv returned, and with it the socket's last hold: libc's copy of
    // `toyos`, which holds the stream's end, lets it go, so its waker of the
    // bell netstack watches it on closes while the child lives.
    let until = toyos_abi::clock::nanos_since_boot() + CEILING_NANOS;
    let hung_up = loop {
        let left = until.saturating_sub(toyos_abi::clock::nanos_since_boot());
        match rung.wait_for(left) {
            Ok(true) => continue,
            Ok(false) => break false,
            Err(Hungup) => break true,
        }
    };
    writeln!(tell, "exit").expect("tell the child to exit");
    let status = child.wait().expect("wait the child");
    assert!(hung_up, "the stream's end outlived its descriptor's close and its last call, in a child that lives on");
    assert!(status.success(), "the {role} ended {status:?}");
}

/// Whether the child's reader looked at its stream within the ceiling, since
/// its want was cleared: a look that finds the ring empty arms the want before
/// it waits. An arm rings nobody, so the word is looked at.
fn looked(words: &Words) -> bool {
    let until = toyos_abi::clock::nanos_since_boot() + CEILING_NANOS;
    while toyos_abi::clock::nanos_since_boot() < until {
        if words[RECEIVE_WANT].load(Ordering::Acquire) != 0 {
            return true;
        }
        std::thread::yield_now();
    }
    false
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

/// Connects through libc, receives on one thread, and closes on this one and
/// gives the number to a pipe; `handling`, with a handler kept.
fn child(handling: bool) {
    if handling {
        let action = SigAction { handler: kept as extern "C" fn(i32) as usize, flags: SA_RESTART, restorer: 0, mask: 0 };
        // SAFETY: a `struct sigaction` as `<signal.h>` lays it out.
        assert_eq!(unsafe { sigaction(SIGCHLD, &action, std::ptr::null_mut()) }, 0, "sigaction");
    }
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
    // The write end is kept, so the pipe the number names is never ready.
    let mut ends = [0i32; 2];
    // SAFETY: libc's calls, the first given room for two descriptors.
    assert_eq!(unsafe { pipe(ends.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: as above.
    assert_eq!(unsafe { dup2(ends[0], fd) }, fd, "dup2 onto the closed number");
    if handling {
        // This thread blocks the signal, so the reader takes it, and its wait
        // starts again after the handler, past the number's reuse.
        let mut ran = [0i32; 2];
        // SAFETY: libc's calls: room for two descriptors, a mask, a byte.
        unsafe {
            assert_eq!(pipe(ran.as_mut_ptr()), 0, "pipe");
            RAN.store(ran[1], Ordering::Relaxed);
            let blocked = 1u64 << (SIGCHLD - 1);
            assert_eq!(sigprocmask(SIG_BLOCK, &blocked, std::ptr::null_mut()), 0, "sigprocmask");
            assert_eq!(raise(SIGCHLD), 0, "raise");
            let mut byte = 0u8;
            assert_eq!(read(ran[0], &mut byte, 1), 1, "the handler's byte");
        }
    }
    say("closed");
    reader.join().expect("the reader");
    assert_eq!(line(&mut orders), "exit");
}
