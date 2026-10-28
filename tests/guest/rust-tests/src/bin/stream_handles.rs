//! A stream's holders across processes, on the real kernel: what host tests
//! and the loom models of `toyos::transport::session` cannot show.
//!
//! - **A holder killed inside the gate leaves the others working.** A
//!   child adopts an exported stream and faults copying its write, holding
//!   the send ring's gate. The kernel ends it and only then hangs its
//!   connection up, and the gate still names it. The parent's write
//!   meanwhile finds the gate held and answers `WouldBlock`, its wants armed;
//!   the departure frees the gate and rings it, and its write and read work.
//! - **netstack's exit ends a blocked read in every holder** with `Aborted`.
//! - **Within a process, every handle of a stream is one end**: handles given
//!   out and taken back read on where the last read stopped; a bare `dup` is
//!   refused by name with nothing written; a stream is no file; `endow` and
//!   `provide` of a stream's own handle are refused, where a descriptor goes
//!   as an exported holder.
//! - **One end, two threads**: a write on one thread completes while another
//!   is in a read the peer has not answered, and that read then takes what
//!   the peer publishes. `sshserver` reads each connection on one thread and
//!   writes it on another.
//! - **Piped children across threads**: one thread writes a child's stdin,
//!   two read its stdout and stderr, and another waits for its exit; a child
//!   that holds its pipes open is killed from a thread other than the one
//!   reading them, and that read ends. `sshserver` runs every program so.
//!
//! **This test is netstack for its children**: a port of its own served to
//! each as `netstack`, which answers a connect with the region judged and two
//! bells, an export with a holder made on its private port, and a departure
//! with `Server::departed`, so no NIC and no peer is needed. Every step waits
//! on an event: a line a child says, a child's exit, a connection's frame or
//! its hang-up, each under a failure ceiling and never a fixed delay. It runs
//! in a guest because the claims are the kernel's: when a dead process's
//! handles close, and what its fault ends.
//!
//! **Its control** is the SDK's `transport-departure-no-pass`, a departure
//! that frees the gate and rings nobody: the parent, registered for writing
//! as mio registers, then watches its send bell to the ceiling and says so.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::net::TcpStreamExt;
use std::os::toyos::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;

use toyos::ipc::{FrameRx, RxStep};
use toyos::net::stream::{
    Adopt, Misuse, Server, Words, ADOPT, EXPORT, EXPORTED, LAYOUT, RECEIVE_RING, REGION_BYTES, RING, SEND_RING,
    SERVER_WATCH,
};
use toyos::net::{MsgType, RespType, TcpConnectRequest, TcpConnectResponse};
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::transport::session::{Holder, Rings};
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

/// The send ring's gate (`toyos::net::stream::LAYOUT`: word 12, each word on
/// a 64-byte line), which a holder that dies inside a write holds.
const TX_GATE: usize = 12 * 16;

/// The failure ceiling of every wait: a liveness bound, not a timing.
const CEILING_NANOS: u64 = 20_000_000_000;

/// The descriptor a child is given its holder of the stream under.
const HELD_FD: u32 = 3;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("client") => client(),
        Some("foreign") => foreign(),
        Some("as-file") => as_file(),
        Some("parent") => parent(),
        Some("faulter") => faulter(),
        Some("reader") => reader(),
        Some("reader-child") => reader_child(),
        Some("two-threads") => two_threads(),
        Some("echo") => echo(),
        Some("hold") => hold(),
        Some("child") => std::process::exit(0),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    a_holder_killed_inside_the_gate_leaves_the_parent_working();
    netstacks_exit_ends_a_blocked_read_in_every_holder();
    shares_within_a_process_and_refuses_moves();
    refuses_a_foreign_handle_writing_nothing();
    refuses_a_stream_as_a_file();
    reads_and_writes_on_two_threads();
    pipes_across_threads();
    println!("a stream's holders: one dead inside the gate leaves the other working; netstack's exit ends both");
}

// ---- the claims ----

/// The parent said `said` where `what` was owed: how it ended, and its stderr.
fn parent_failed(parent: &mut Child, what: &str, said: &str) -> ! {
    let mut why = String::new();
    let _ = parent.stderr.take().expect("the parent's stderr").read_to_string(&mut why);
    panic!("{what}: the parent said {said:?}, ended {:?}, and wrote {why}", parent.wait());
}

/// A dead holder's hang-up comes after its last instruction, and the parent
/// works on through the departure, step by awaited step.
fn a_holder_killed_inside_the_gate_leaves_the_parent_working() {
    let (mut parent, mut netstack) = run("parent", &[]);
    let mut out = BufReader::new(parent.stdout.take().expect("the parent's stdout"));
    netstack.serve_export(Holder::FIRST);
    let said = line(&mut out);
    if !(said.starts_with("the child ended") && said.contains("Fault(Memory)")) {
        parent_failed(&mut parent, "the faulting child", &said);
    }
    assert_eq!(line(&mut out), "contended", "the parent's write did not find the gate held");
    let second = Holder::new(2).expect("a holder's number");
    netstack.hung_up(second);
    let gate = netstack.words()[TX_GATE].load(Ordering::Acquire);
    assert_eq!(gate & 0xFFFF, 2, "the child died outside the gate: it reads {gate:#x}");
    netstack.depart(second);
    let said = line(&mut out);
    if said != "wrote" {
        parent_failed(&mut parent, "the parent's write after the departure", &said);
    }
    assert_eq!(netstack.sent(), b"p", "what the parent's write published");
    netstack.publish(b"ok");
    assert_eq!(line(&mut out), "read ok", "the parent's read after the departure");
    let status = parent.wait().expect("wait the parent");
    assert!(status.success(), "the parent ended {status:?}");
    println!("  a holder killed inside the gate: its hang-up came after it, and the parent wrote and read");
}

/// Two holders blocked in a read, and netstack gone.
fn netstacks_exit_ends_a_blocked_read_in_every_holder() {
    let (mut reader, mut netstack) = run("reader", &[]);
    let mut out = BufReader::new(reader.stdout.take().expect("the reader's stdout"));
    netstack.serve_export(Holder::FIRST);
    let mut reading = [line(&mut out), line(&mut out)];
    reading.sort();
    assert_eq!(reading, ["reading child", "reading parent"]);
    drop(netstack);
    let mut ended = [line(&mut out), line(&mut out)];
    ended.sort();
    assert_eq!(ended, ["child: ConnectionAborted", "parent: ConnectionAborted"], "a read netstack's exit did not end");
    let status = reader.wait().expect("wait the reader");
    assert!(status.success(), "the reader ended {status:?}");
    println!("  netstack's exit: a blocked read in each holder answered ConnectionAborted");
}

/// The handles given out and taken back share one end, and a stream's own
/// handle is neither endowed nor provided.
fn shares_within_a_process_and_refuses_moves() {
    let (mut child, netstack) = run("client", PEER);
    let out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    let said: Vec<String> = out.lines().map(|line| line.expect("a line")).collect();
    let status = child.wait().expect("wait the client");
    assert_eq!(said, ["shared", "endow refused", "provide refused"], "the client said otherwise");
    assert!(status.success(), "the client exited {status:?}");
    println!("  given out and taken back: one end; its own handle endowed or provided: refused");
    hang_up_answers_the_server_watch(&netstack.holders[0].connection);
}

/// netstack's watch of a holder's connection, made after its client left:
/// the kernel takes it and answers it, and the connection then reads its end.
fn hang_up_answers_the_server_watch(conn: &Connection) {
    let poller = Poller::new(1);
    poller.watch_raw(conn.as_handle(), SERVER_WATCH, 0);
    let mut answers = Vec::new();
    poller.wait_answers(1, CEILING_NANOS, |_, answer| answers.push(answer));
    assert!(matches!(answers[..], [Ok(_)]), "the kernel answered netstack's watch {answers:?}");
    let mut bytes = [0u8; 64];
    while syscall::read_nonblock(conn.as_handle(), &mut bytes).is_ok_and(|n| n > 0) {}
    assert_eq!(syscall::read_nonblock(conn.as_handle(), &mut bytes), Ok(0), "the hang-up is the connection's end");
    println!("  netstack's watch of a holder's connection: taken, and answered by the hang-up");
}

/// A bare `dup` of a stream's handle is no stream of the process: refused by
/// name, with nothing written on its connection before the process ends.
fn refuses_a_foreign_handle_writing_nothing() {
    let (mut child, netstack) = run("foreign", &[]);
    let conn = &netstack.holders[0].connection;
    let mut out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    assert_eq!(line(&mut out), "duplicated");
    // What the connect may have rung, taken; from here the connection carries nothing but its end.
    let mut bytes = [0u8; 64];
    while syscall::read_nonblock(conn.as_handle(), &mut bytes).is_ok_and(|n| n > 0) {}
    child.stdin.take().expect("the child's stdin").write_all(b"go\n").expect("tell the child");
    let said = child.wait_with_output().expect("wait the child");
    let stderr = String::from_utf8_lossy(&said.stderr);
    assert!(!said.status.success(), "the foreign handle was taken: {}", String::from_utf8_lossy(&said.stdout));
    assert!(stderr.contains("is no stream of this process's"), "refused, but not by name: {stderr}");
    let read = syscall::read(conn.as_handle(), &mut bytes);
    assert_eq!(read, Ok(0), "the refusal wrote on the stream's connection before its end");
    println!("  a foreign handle: refused by name, nothing written");
}

/// `File::from_raw_fd` of a stream's handle panics with the refusal.
fn refuses_a_stream_as_a_file() {
    let (child, _netstack) = run("as-file", &[]);
    let said = child.wait_with_output().expect("wait the child");
    let stderr = String::from_utf8_lossy(&said.stderr);
    assert!(!said.status.success(), "the stream was taken as a file: {}", String::from_utf8_lossy(&said.stdout));
    assert!(stderr.contains("is a stream's, which is read and written through its region, not as a file"), "refused, but not by name: {stderr}");
    println!("  a stream as a file: refused by name");
}

// ---- this test as netstack ----

/// What the client writes while its other thread reads.
const WRITTEN: &[u8] = b"written while the other thread reads";

/// What the peer publishes once the write is in.
const LATER: &[u8] = b"published after the write";

/// A write on one thread completes while the other thread's read waits on
/// the peer, and the read then takes what the peer publishes after it.
fn reads_and_writes_on_two_threads() {
    let (mut child, mut netstack) = run("two-threads", PEER);
    let mut out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    assert_eq!(line(&mut out), "reading");
    assert_eq!(line(&mut out), "written");
    assert_eq!(netstack.sent(), WRITTEN, "the write is in the send ring before the read is answered");
    let words = words_of(&netstack.region);
    let released = netstack.server.release(words, WRITTEN.len() as u32).expect("the client keeps the protocol");
    netstack.region.bytes().write(RECEIVE_RING + PEER.len(), LATER);
    let published = netstack.server.publish(words, LATER.len() as u32).expect("the client keeps the protocol");
    netstack.ring(released.and(published));
    assert_eq!(line(&mut out), "read");
    let status = child.wait().expect("wait the client");
    assert!(status.success(), "the client exited {status:?}");
    println!("  one end, two threads: the write completes beside an unanswered read, which then takes the peer's next bytes");
}

/// A client that reads on one thread and writes on another.
fn two_threads() -> ! {
    let mut stream = connect();
    let mut reader = stream.try_clone().expect("a second handle");
    let (entered, entering) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || {
        let mut peer = vec![0u8; PEER.len()];
        reader.read_exact(&mut peer).expect("the peer's first bytes");
        assert_eq!(peer, PEER);
        say("reading");
        entered.send(()).expect("the writer waits");
        let mut later = vec![0u8; LATER.len()];
        reader.read_exact(&mut later).expect("the peer's later bytes");
        assert_eq!(later, LATER);
    });
    entering.recv().expect("the reader is reading");
    stream.write_all(WRITTEN).expect("a write beside the read");
    say("written");
    reading.join().expect("the reader");
    say("read");
    std::process::exit(0)
}

/// What `echo` is fed: more than a pipe holds, so writer and readers wait on
/// each other.
const FED: usize = 256 * 1024;

/// A child's stdin written, its stdout and stderr read and its exit waited
/// for, each on a thread of its own; then a child holding its pipes open
/// killed from one thread while another reads its stdout.
fn pipes_across_threads() {
    use std::sync::{Arc, Mutex};
    let fed: Vec<u8> = (0..FED).map(|i| (i % 251) as u8).collect();
    let mut child = Command::new(SELF_PATH).arg("echo").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn echo");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let writer = {
        let fed = fed.clone();
        std::thread::spawn(move || stdin.write_all(&fed).expect("feed echo"))
    };
    let out = std::thread::spawn(move || {
        let mut all = Vec::new();
        stdout.read_to_end(&mut all).expect("echo's stdout");
        all
    });
    let err = std::thread::spawn(move || {
        let mut all = String::new();
        stderr.read_to_string(&mut all).expect("echo's stderr");
        all
    });
    writer.join().expect("the writer");
    let (out, err) = (out.join().expect("the stdout reader"), err.join().expect("the stderr reader"));
    let status = std::thread::spawn(move || child.wait().expect("wait echo")).join().expect("the waiter");
    assert!(out == fed, "echo's stdout is its stdin: {} bytes of {}", out.len(), fed.len());
    assert_eq!((err.as_str(), status.code()), ("fed\n", Some(7)));

    let mut child = Command::new(SELF_PATH).arg("hold").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().expect("spawn hold");
    let mut stdout = child.stdout.take().expect("stdout");
    let (greeted, greeting) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = [0u8; 5];
        stdout.read_exact(&mut line).expect("hold's greeting");
        greeted.send(()).expect("the killer waits");
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).expect("hold's stdout to its end");
        rest
    });
    let child = Arc::new(Mutex::new(child));
    let killer = child.clone();
    greeting.recv().expect("hold is running");
    std::thread::spawn(move || killer.lock().expect("the child").kill().expect("kill hold")).join().expect("the killer");
    assert_eq!(reader.join().expect("the reader"), b"", "the killed child's stdout ends");
    let status = child.lock().expect("the child").wait().expect("wait hold");
    assert!(!status.success(), "hold was killed: {status:?}");
    println!("  piped children across threads: fed, read and waited for apart, and one killed beside its reader");
}

/// Its stdin back on stdout, to its end, then a word on stderr and exit 7.
fn echo() -> ! {
    let mut all = Vec::new();
    std::io::stdin().read_to_end(&mut all).expect("stdin");
    std::io::stdout().write_all(&all).expect("stdout");
    std::io::stdout().flush().expect("flush");
    eprint!("fed\n");
    std::process::exit(7)
}

/// A greeting, then its pipes held open until it is killed.
fn hold() -> ! {
    print!("held\n");
    std::io::stdout().flush().expect("flush");
    let mut never = String::new();
    let _ = std::io::stdin().read_line(&mut never);
    panic!("hold's stdin ended: nothing closes it")
}

/// One holder as this test keeps it: its connection and the write ends of
/// its two bells.
struct Holding {
    number: Holder,
    connection: Connection,
    frames: FrameRx<0>,
    receive: Pipe,
    send: Pipe,
}

/// A stream's server end as this test plays netstack, kept for the test's
/// step: dropped, every holder's connection and bells hang up.
struct Netstack {
    region: SharedMemory,
    server: Server,
    holders: Vec<Holding>,
    /// The private port every holder past the first is a connection of.
    exports: (port::Acceptor, namespace::Namespace),
}

/// The region's header.
fn words_of(region: &SharedMemory) -> &Words {
    // SAFETY: the mapping is `REGION_BYTES`, 2 MiB aligned, and its header is reached only as atomics.
    unsafe { &*(region.as_ptr() as *const Words) }
}

impl Netstack {
    fn words(&self) -> &Words {
        words_of(&self.region)
    }

    fn holder(&mut self, number: Holder) -> &mut Holding {
        self.holders.iter_mut().find(|h| h.number == number).expect("a holder this test made")
    }

    /// Every holder's bells `rings` names.
    fn ring(&self, rings: Rings) {
        for holding in &self.holders {
            if rings.receive {
                let _ = holding.receive.write_nonblock(&[1]);
            }
            if rings.send {
                let _ = holding.send.write_nonblock(&[1]);
            }
        }
    }

    /// The next frame on `number`'s connection that is no ring, or `None` for
    /// its hang-up, each wait for one under the ceiling.
    fn next_frame(&mut self, number: Holder) -> Option<u32> {
        let holding = self.holder(number);
        loop {
            match holding.frames.pump(&holding.connection) {
                RxStep::Frame { msg_type: RING, .. } => continue,
                RxStep::Frame { msg_type, .. } => return Some(msg_type),
                RxStep::Eof => return None,
                RxStep::Malformed => panic!("a holder wrote a frame no holder writes"),
                RxStep::Idle => {}
            }
            let poller = Poller::new(1);
            poller.watch_raw(holding.connection.as_handle(), SERVER_WATCH, 0);
            let mut answered = false;
            poller.wait(1, CEILING_NANOS, |_| answered = true);
            assert!(answered, "a holder said nothing within the ceiling");
        }
    }

    /// `asker`'s export, answered with a holder of the stream made on the
    /// private port, its `ADOPT` queued first.
    fn serve_export(&mut self, asker: Holder) {
        assert_eq!(self.next_frame(asker), Some(EXPORT), "the spawn asks for a holder");
        let number = Holder::new(self.holders.len() as u16 + 1).expect("a holder's number");
        let theirs = self.exports.1.open("export").expect("a connection of the private port");
        let ours = self.exports.0.accept().expect("the private port's connection");
        let (receive_read, receive) = toyos::pipe_pair().expect("a receive bell");
        let (send_read, send) = toyos::pipe_pair().expect("a send bell");
        let adopt = Adopt { holder: number.number(), peer_port: 80, peer_addr: [192, 0, 2, 2], local_port: 49_152, _pad: 0 };
        let region = self.region.share().expect("a handle of the region");
        let handles = [region, OwnedHandle::from(receive_read), OwnedHandle::from(send_read)];
        ours.send_with(ADOPT, &adopt, handles).expect("queue the holder's adoption");
        self.holders.push(Holding { number, connection: ours, frames: FrameRx::new(), receive, send });
        self.holder(asker).connection.signal_with(EXPORTED, [theirs.into_owned()]).expect("answer the export");
    }

    /// Waits for `number`'s hang-up.
    fn hung_up(&mut self, number: Holder) {
        assert_eq!(self.next_frame(number), None, "the holder said more before it hung up");
    }

    /// `number` left: the gates it held let go, and every holder rung as
    /// `Server::departed` answers.
    fn depart(&mut self, number: Holder) {
        let rings = self.server.departed(words_of(&self.region), number).expect("the holders keep the protocol");
        self.holders.retain(|h| h.number != number);
        self.ring(rings);
    }

    /// The bytes the holders published.
    fn sent(&mut self) -> Vec<u8> {
        let n = self.server.sent(words_of(&self.region)).expect("the holders keep the protocol") as usize;
        self.region.bytes().sub(SEND_RING, n).to_vec()
    }

    /// `bytes` published into an empty receive ring, and the holders rung.
    fn publish(&mut self, bytes: &[u8]) {
        self.region.bytes().write(RECEIVE_RING, bytes);
        let rings = self.server.publish(words_of(&self.region), bytes.len() as u32).expect("the holders keep the protocol");
        self.ring(rings);
    }
}

/// Spawns `role` with this test as its netstack, and serves its connect, the
/// stream's receive ring holding `peer`.
fn run(role: &str, peer: &[u8]) -> (Child, Netstack) {
    let (acceptor, connector) = port::create().expect("a port");
    // This test's own names, so a child reaches the file server its own
    // spawns read images through, and this test as its `netstack`.
    let own = toyos::endow::namespace().expect("this test's namespace");
    let ns = namespace::build().keep_all(own).add(SERVICE, &connector).finish().expect("a namespace");
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
    // SAFETY: received with this frame, so this process's and nobody else's.
    let region = SharedMemory::lent(unsafe { OwnedHandle::from_raw(region) }, REGION_BYTES).expect("a region netstack takes");
    let mut server = Server::new(words_of(&region), LAYOUT);
    region.bytes().write(RECEIVE_RING, peer);
    let _ = server.publish(words_of(&region), peer.len() as u32).expect("the client keeps the protocol");
    let (receive_read, receive) = toyos::pipe_pair().expect("the receive bell");
    let (send_read, send) = toyos::pipe_pair().expect("the send bell");
    let bells = [OwnedHandle::from(receive_read), OwnedHandle::from(send_read)];
    conn.send_with(RespType::Result as u32, &TcpConnectResponse { local_port: 49_152, _pad: 0 }, bells)
        .expect("answer the connect with the bells");
    let (exports, export_connector) = port::create().expect("the private port");
    let names = namespace::build().add("export", &export_connector).finish().expect("the private port's namespace");
    let first = Holding { number: Holder::FIRST, connection: conn, frames: FrameRx::new(), receive, send };
    (child, Netstack { region, server, holders: vec![first], exports: (exports, names) })
}

fn line(out: &mut BufReader<ChildStdout>) -> String {
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

// ---- the children ----

/// Connects, gives a child a holder that faults inside a write, then writes
/// and reads through what the child left.
fn parent() -> ! {
    let mut stream = connect();
    let handle = stream.as_raw_handle();
    let child = Command::new(SELF_PATH).arg("faulter").inherit_handle(HELD_FD, handle).stdout(Stdio::null()).spawn();
    let status = child.expect("spawn the faulter with a holder of the stream").wait().expect("wait the faulter");
    say(&format!("the child ended {status:?}"));
    stream.set_nonblocking(true).expect("nonblocking");
    match stream.write(b"p") {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => say("contended"),
        other => {
            say(&format!("the write answered {other:?} under the dead child's gate"));
            std::process::exit(1);
        }
    }
    // As mio waits, through std: registered for writing, then the send bell
    // watched until the departure rings it. (This binary's own copy of
    // `toyos` holds no end of std's stream.)
    stream.register(false, true).expect("register the stream for writing");
    let bell = toyos_abi::RawHandle(stream.bell().expect("the send bell"));
    let poller = Poller::new(1);
    poller.watch_raw(bell, READABLE, 0);
    let mut rung = false;
    poller.wait(1, CEILING_NANOS, |_| rung = true);
    if !rung {
        say("the send bell was never rung after the departure");
        std::process::exit(1);
    }
    stream.write_all(b"p").expect("the write after the departure");
    say("wrote");
    stream.set_nonblocking(false).expect("blocking");
    let mut got = [0u8; 2];
    stream.read_exact(&mut got).expect("the read after the departure");
    say(&format!("read {}", String::from_utf8_lossy(&got)));
    std::process::exit(0)
}

/// Adopts its holder and writes from memory that is not mapped: the copy
/// faults inside the send ring's gate, and the kernel ends the process.
fn faulter() -> ! {
    // SAFETY: the descriptor its parent gave it, a holder of the stream.
    let mut stream = unsafe { TcpStream::from_raw_fd(HELD_FD as i32) };
    let region = SharedMemory::create(2 << 20).expect("memory to let go of");
    let gone = region.as_ptr();
    drop(region);
    // SAFETY: none: the read of `gone` is the fault this role exists to take.
    let unmapped = unsafe { std::slice::from_raw_parts(gone, 1) };
    let _ = stream.write(unmapped);
    unreachable!("a write from unmapped memory returned")
}

/// Gives a child a holder, then both read until netstack is gone.
fn reader() -> ! {
    let mut stream = connect();
    let mut child = Command::new(SELF_PATH)
        .arg("reader-child")
        .inherit_handle(HELD_FD, stream.as_raw_handle())
        .spawn()
        .expect("spawn the reader's child with a holder of the stream");
    say("reading parent");
    let ended = stream.read(&mut [0u8; 8]).map(|n| n.to_string()).unwrap_or_else(|e| format!("{:?}", e.kind()));
    say(&format!("parent: {ended}"));
    let status = child.wait().expect("wait the reader's child");
    std::process::exit(if status.success() { 0 } else { 1 })
}

fn reader_child() -> ! {
    // SAFETY: the descriptor its parent gave it, a holder of the stream.
    let mut stream = unsafe { TcpStream::from_raw_fd(HELD_FD as i32) };
    say("reading child");
    let ended = stream.read(&mut [0u8; 8]).map(|n| n.to_string()).unwrap_or_else(|e| format!("{:?}", e.kind()));
    say(&format!("child: {ended}"));
    std::process::exit(0)
}

/// The next `n` bytes the stream reads, which must be the peer's from `at`.
fn reads(stream: &mut TcpStream, at: usize, n: usize) {
    let mut got = vec![0u8; n];
    stream.read_exact(&mut got).expect("the peer's bytes");
    assert_eq!(got, &PEER[at..at + n], "read from where the last read stopped");
}

/// A client whose stream's handle is given out three ways and taken back,
/// then endowed and provided.
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
    let paths: [(&str, fn(&mut Command, u32)); 2] = [
        ("endow", |command, handle| {
            command.endow("stream", handle);
        }),
        ("provide", |command, handle| {
            command.provide("stream", handle);
        }),
    ];
    for (path, moving) in paths {
        let mut command = Command::new(SELF_PATH);
        command.arg("child").stdout(Stdio::null());
        moving(&mut command, handle);
        match command.spawn() {
            Err(refused)
                if refused.kind() == std::io::ErrorKind::InvalidInput
                    && refused.to_string() == Misuse::Moved(RawHandle(handle)).to_string() =>
            {
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
