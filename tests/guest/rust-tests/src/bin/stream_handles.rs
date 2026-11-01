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
//! - **One `Command` spawned twice gives each child a holder of its own**:
//!   each spawn asks netstack for one, and both children write through their
//!   own.
//! - **Within a process, every handle of a stream is one end**: handles given
//!   out and taken back read on where the last read stopped; a bare `dup` is
//!   refused by name with nothing written; a stream is no file; a duplicate of
//!   a stream's handle, endowed or sent, is refused by the kernel, which moves
//!   no handle without `TRANSFER`, where a descriptor goes as an exported
//!   holder.
//! - **One end, two threads**: a write on one thread completes while another
//!   is in a read the peer has not answered, and that read then takes what
//!   the peer publishes. `sshserver` reads each connection on one thread and
//!   writes it on another.
//! - **Piped children across threads**: one thread writes a child's stdin,
//!   two read its stdout and stderr, and another waits for its exit; a child
//!   that holds its pipes open is killed from a thread other than the one
//!   reading them, and that read ends. `sshserver` runs every program so.
//! - **A stream's local address is netstack's answer**: std's `local_addr`
//!   reads the address and port the connect's answer named, and a connect
//!   whose answer names the unspecified address is refused.
//!
//! **This test is netstack for its children**: a port of its own served to
//! each as `netstack`, which answers a connect with the region judged and its
//! bells, an export with a holder made on its private port, and a departure
//! with `Server::departed`, so no NIC and no peer is needed. Every step waits
//! on an event: a line a child says, a child's exit, a holder's ring, frame or
//! leaving, each under a failure ceiling and never a fixed delay. It runs
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
use std::os::toyos::net::TcpStreamExt;
use std::os::toyos::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;

use toyos::ipc::{FrameRx, IpcError, RxStep};
use toyos::net::stream::{Adopt, Server, Words, ADOPT, EXPORT, EXPORTED, LAYOUT, RECEIVE_RING, REGION_BYTES, SEND_RING};
use toyos::net::{MsgType, RespType, TcpConnectRequest, TcpConnectResponse};
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::transport::session::{Holder, Rings};
use toyos::wake::{self, Bell, Hungup, Waker};
use toyos::{namespace, port, AsHandle, Connection, OwnedHandle};
use toyos_abi::syscall::{self, SyscallError, SVC_LABEL};

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
        Some("respawner") => respawner(),
        Some("writer-child") => writer_child(),
        Some("two-threads") => two_threads(),
        Some("echo") => echo(),
        Some("hold") => hold(),
        Some("local") => local(),
        Some("unaddressed") => unaddressed(),
        Some("child") => std::process::exit(0),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    a_holder_killed_inside_the_gate_leaves_the_parent_working();
    netstacks_exit_ends_a_blocked_read_in_every_holder();
    one_command_gives_each_spawn_a_holder_of_its_own();
    shares_within_a_process_and_refuses_moves();
    refuses_a_foreign_handle_writing_nothing();
    refuses_a_stream_as_a_file();
    reads_and_writes_on_two_threads();
    pipes_across_threads();
    takes_its_local_address_from_the_answer();
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

/// One `Command` bound to the stream and spawned twice: each spawn asks for a
/// holder of its own, and each child writes through it.
fn one_command_gives_each_spawn_a_holder_of_its_own() {
    let (mut parent, mut netstack) = run("respawner", &[]);
    let mut out = BufReader::new(parent.stdout.take().expect("the parent's stdout"));
    for n in 0..2 {
        let holder = netstack.serve_export(Holder::FIRST);
        let said = line(&mut out);
        if said != format!("child {n} wrote") {
            parent_failed(&mut parent, "a child of the one command", &said);
        }
        netstack.hung_up(holder);
        netstack.depart(holder);
    }
    assert_eq!(netstack.sent(), b"cc", "what the two children published");
    let status = parent.wait().expect("wait the parent");
    assert!(status.success(), "the parent ended {status:?}");
    println!("  one command spawned twice: each child had a holder of its own, and both wrote");
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

/// The handles given out and taken back share one end, and a duplicate of
/// its handle is neither endowed nor sent.
fn shares_within_a_process_and_refuses_moves() {
    let (mut child, netstack) = run("client", PEER);
    let out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    let said: Vec<String> = out.lines().map(|line| line.expect("a line")).collect();
    let status = child.wait().expect("wait the client");
    assert_eq!(said, ["shared", "endow refused", "send refused"], "the client said otherwise");
    assert!(status.success(), "the client exited {status:?}");
    println!("  given out and taken back: one end; a duplicate of its handle endowed or sent: refused");
    departure_answers_the_holders_bell(&netstack.holders[0].rung);
}

/// netstack's watch of a holder's bell, made after its client left: the
/// kernel takes it and answers it, and the bell then reads its wakers' end.
fn departure_answers_the_holders_bell(rung: &Bell) {
    let poller = Poller::new(1);
    poller.watch(rung, READABLE, 0);
    let mut answers = Vec::new();
    poller.wait_answers(1, CEILING_NANOS, |_, answer| answers.push(answer));
    assert!(matches!(answers[..], [Ok(_)]), "the kernel answered netstack's watch {answers:?}");
    while rung.try_take() == Ok(true) {}
    assert_eq!(rung.try_take(), Err(Hungup), "the departure is the bell's wakers' end");
    println!("  netstack's watch of a holder's bell: taken, and answered by its departure");
}

/// A bare `dup` of a stream's handle is no stream of the process: refused by
/// name, with nothing written on its connection nor rung before the process
/// ends.
fn refuses_a_foreign_handle_writing_nothing() {
    let (mut child, netstack) = run("foreign", &[]);
    let (conn, rung) = (&netstack.holders[0].connection, &netstack.holders[0].rung);
    let mut out = BufReader::new(child.stdout.take().expect("the child's stdout"));
    assert_eq!(line(&mut out), "duplicated");
    // What the connect may have rung, taken; from here the bell rings nothing but its end.
    while rung.try_take() == Ok(true) {}
    let mut bytes = [0u8; 64];
    child.stdin.take().expect("the child's stdin").write_all(b"go\n").expect("tell the child");
    let said = child.wait_with_output().expect("wait the child");
    let stderr = String::from_utf8_lossy(&said.stderr);
    assert!(!said.status.success(), "the foreign handle was taken: {}", String::from_utf8_lossy(&said.stdout));
    assert!(stderr.contains("is no stream of this process's"), "refused, but not by name: {stderr}");
    let taken = syscall::message_recv(conn.as_handle().raw(), &mut [], &mut bytes, &mut [], false);
    assert_eq!(taken, Err(SyscallError::Gone), "the refusal sent on the stream's connection before its end");
    assert_eq!(rung.try_take(), Err(Hungup), "the refusal rang the stream before its end");
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

/// std's `local_addr` is what the connect's answer named, and an answer naming
/// the unspecified address refuses the connect.
fn takes_its_local_address_from_the_answer() {
    let (child, _netstack) = run("local", &[]);
    let said = child.wait_with_output().expect("wait the child");
    assert_eq!(String::from_utf8_lossy(&said.stdout).trim(), "192.0.2.1:49152", "the stream's local address");
    let (child, _netstack) = run_answering("unaddressed", &[], [0; 4]);
    let said = child.wait_with_output().expect("wait the child");
    assert_eq!(String::from_utf8_lossy(&said.stdout).trim(), "refused", "a connect answered with no local address");
    println!("  a stream's local address: the answer's, and none refused");
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

/// One holder as this test keeps it: its connection, the wakers of its two
/// bells, and the bell it rings.
struct Holding {
    number: Holder,
    connection: Connection,
    frames: FrameRx<0>,
    receive: Waker,
    send: Waker,
    rung: Bell,
}

/// A holder's three bells: what it is handed, in `BELLS`'s order, and the
/// wakers and bell this test keeps.
fn bells() -> ([OwnedHandle; 3], (Waker, Waker, Bell)) {
    let (receive, receive_bell) = wake::pair().expect("a receive bell");
    let (send, send_bell) = wake::pair().expect("a send bell");
    let (rung_waker, rung) = wake::pair().expect("a rung bell");
    ([receive_bell.into(), send_bell.into(), rung_waker.into()], (receive, send, rung))
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
                holding.receive.wake();
            }
            if rings.send {
                holding.send.wake();
            }
        }
    }

    /// The next frame on `number`'s connection, or `None` for its leaving,
    /// each wait for its bell under the ceiling.
    fn next_frame(&mut self, number: Holder) -> Option<u32> {
        let holding = self.holder(number);
        loop {
            match holding.frames.pump(&holding.connection) {
                RxStep::Frame { msg_type, .. } => return Some(msg_type),
                RxStep::Eof => return None,
                RxStep::Malformed => panic!("a holder wrote a frame no holder writes"),
                RxStep::Idle => {}
            }
            match holding.rung.wait_for(CEILING_NANOS) {
                Ok(true) => {}
                Ok(false) => panic!("a holder said nothing within the ceiling"),
                Err(Hungup) => return None,
            }
        }
    }

    /// `asker`'s export, answered with a holder of the stream made on the
    /// private port, its `ADOPT` queued first: the holder's number.
    fn serve_export(&mut self, asker: Holder) -> Holder {
        assert_eq!(self.next_frame(asker), Some(EXPORT), "the spawn asks for a holder");
        let number = Holder::new(self.holders.len() as u16 + 1).expect("a holder's number");
        let theirs = self.exports.1.open("export").expect("a connection of the private port");
        let ours = self.exports.0.accept().expect("the private port's connection");
        let ([receive_bell, send_bell, rung_waker], (receive, send, rung)) = bells();
        let adopt = Adopt { holder: number.number(), peer_port: 80, peer_addr: [192, 0, 2, 2], local_addr: [192, 0, 2, 1], local_port: 49_152, _pad: 0 };
        let region = self.region.share().expect("a handle of the region");
        let handles = [region, receive_bell, send_bell, rung_waker];
        ours.send_with(ADOPT, &adopt, handles).expect("queue the holder's adoption");
        self.holders.push(Holding { number, connection: ours, frames: FrameRx::new(), receive, send, rung });
        self.holder(asker).connection.signal_with(EXPORTED, [theirs.into_owned()]).expect("answer the export");
        number
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
    run_answering(role, peer, [192, 0, 2, 1])
}

/// [`run`], the connect answered as left from `local`, port 49152.
fn run_answering(role: &str, peer: &[u8], local: [u8; 4]) -> (Child, Netstack) {
    let (acceptor, connector) = port::create().expect("a port");
    // This test's own names, so a child reaches the file server its own
    // spawns read images through, and this test as its `netstack`.
    let own = toyos::endow::namespace().expect("this test's namespace");
    let ns = namespace::build().keep_all(own).add(SERVICE, &connector).finish().expect("a namespace");
    let child = Command::new(SELF_PATH)
        .arg(role)
        .endow(SVC_LABEL, ns.into())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {role}: {e}"));
    let conn = acceptor.accept().expect("the child's connect");
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let frame = rx.recv(&conn).expect("a request");
    assert_eq!(frame.msg_type, MsgType::TcpConnect as u32, "a stream's connect");
    let _: TcpConnectRequest = toyos::ipc::decode_payload(rx.payload(frame.payload_len)).expect("the connect's request");
    let [region] = rx.take_handles::<1>().expect("the stream's region");
    let region = SharedMemory::lent(region, REGION_BYTES).expect("a region netstack takes");
    let mut server = Server::new(words_of(&region), LAYOUT);
    region.bytes().write(RECEIVE_RING, peer);
    let _ = server.publish(words_of(&region), peer.len() as u32).expect("the client keeps the protocol");
    let (handed, (receive, send, rung)) = bells();
    conn.send_with(RespType::Result as u32, &TcpConnectResponse { local_addr: local, local_port: 49_152, _pad: 0 }, handed)
        .expect("answer the connect with the bells");
    let (exports, export_connector) = port::create().expect("the private port");
    let names = namespace::build().add("export", &export_connector).finish().expect("the private port's namespace");
    let first = Holding { number: Holder::FIRST, connection: conn, frames: FrameRx::new(), receive, send, rung };
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
    let handle = std::os::toyos::io::AsHandle::as_handle(&stream);
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
    let bell = stream.bell().expect("the send bell");
    // SAFETY: the stream's send bell, which `stream` holds past the wait.
    let bell = unsafe { std::os::toyos::io::BorrowedHandle::borrow_raw(bell) };
    let poller = Poller::new(1);
    poller.watch(&bell, READABLE, 0);
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
        .inherit_handle(HELD_FD, std::os::toyos::io::AsHandle::as_handle(&stream))
        .spawn()
        .expect("spawn the reader's child with a holder of the stream");
    say("reading parent");
    let ended = stream.read(&mut [0u8; 8]).map(|n| n.to_string()).unwrap_or_else(|e| format!("{:?}", e.kind()));
    say(&format!("parent: {ended}"));
    let status = child.wait().expect("wait the reader's child");
    std::process::exit(if status.success() { 0 } else { 1 })
}

/// Spawns one command, bound to the stream, twice.
fn respawner() -> ! {
    let stream = connect();
    let mut command = Command::new(SELF_PATH);
    command.arg("writer-child").inherit_handle(HELD_FD, std::os::toyos::io::AsHandle::as_handle(&stream)).stdout(Stdio::null());
    for n in 0..2 {
        let status = command.spawn().expect("spawn a child of the one command").wait().expect("wait it");
        say(&match status.success() {
            true => format!("child {n} wrote"),
            false => format!("child {n} ended {status:?}"),
        });
    }
    std::process::exit(0)
}

/// Adopts its holder and writes one byte through it.
fn writer_child() -> ! {
    // SAFETY: the descriptor its parent gave it, a holder of the stream.
    let mut stream = unsafe { TcpStream::from_raw_fd(HELD_FD as i32) };
    std::process::exit(if stream.write_all(b"c").is_ok() { 0 } else { 1 })
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
/// then a duplicate of it endowed and sent.
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

    // A duplicate carries the end's rights, which hold no `TRANSFER`: the
    // kernel refuses to move it, into a child's table or into a frame. Its
    // word reaches std as a bare kind, where the SDK's own refusals carry
    // their reason.
    let copy = || std::os::toyos::io::AsHandle::as_handle(&taken).try_clone_to_owned().expect("a duplicate of the stream's handle");
    let mut command = Command::new(SELF_PATH);
    command.arg("child").stdout(Stdio::null()).endow("stream", copy());
    match command.spawn() {
        Err(refused) if refused.kind() == std::io::ErrorKind::PermissionDenied && refused.get_ref().is_none() => say("endow refused"),
        Err(other) => say(&format!("endow answered {other}")),
        Ok(mut child) => {
            let _ = child.wait();
            say("endow took a duplicate of the stream's handle");
        }
    }
    let (acceptor, connector) = port::create().expect("a port of the client's own");
    let names = namespace::build().add("sink", &connector).finish().expect("its namespace");
    let conn = names.open("sink").expect("a connection to it");
    let _accepted = acceptor.accept().expect("its other end");
    match conn.signal_with(1, [OwnedHandle::from(copy())]) {
        Err(IpcError::Syscall(SyscallError::PermissionDenied)) => say("send refused"),
        other => say(&format!("send answered {other:?}")),
    }
    std::process::exit(0)
}

/// A client that takes a bare `dup` of its stream's handle as a stream.
fn foreign() -> ! {
    let stream = connect();
    let copy = AsHandle::as_handle(&stream).try_clone_to_owned().expect("a bare duplicate");
    let fd = toyos::fd::install(toyos::fd::Object::handle(copy), false, 0).expect("a descriptor for the duplicate");
    say("duplicated");
    let mut go = String::new();
    std::io::stdin().read_line(&mut go).expect("the test's go");
    // SAFETY: a descriptor this process holds and nothing else answers for.
    let _adopted = unsafe { TcpStream::from_raw_fd(fd) };
    say("adopted");
    std::process::exit(0)
}

/// A client that says its stream's local address.
fn local() -> ! {
    let stream = connect();
    say(&stream.local_addr().expect("the stream's local address").to_string());
    std::process::exit(0)
}

/// A client whose connect's answer names no local address.
fn unaddressed() -> ! {
    match TcpStream::connect(ADDR.parse::<SocketAddr>().expect("an address")) {
        Ok(stream) => say(&format!("connected from {:?}", stream.local_addr())),
        Err(_) => say("refused"),
    }
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
