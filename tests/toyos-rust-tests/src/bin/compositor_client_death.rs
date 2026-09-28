//! The desktop must survive a client that dies, and one that asks for
//! something the kernel will refuse on its behalf.
//!
//! The owner's machine lost its whole desktop to this: doom aborted, and three
//! seconds later the compositor granted a resized window's buffer to it —
//! `grant_shared` answered `InvalidArgument` for a pid the process table no
//! longer had, `SharedMemory::grant` was infallible over that, and every other
//! window went with it. `exit: compositor code=101`. There is no grant left to
//! be infallible over: a buffer travels as a handle and a client that has gone
//! is a refused send.
//!
//! Six cases. The first is that one; the next four are the same shape found
//! by reading for it — places where a message from any client reached a
//! syscall or a buffer whose refusal the compositor was not prepared to hear.
//!
//! The fifth is the other side of the same event, and it is the client's:
//! **a window whose connection has gone must let its owner leave.** Nothing
//! else here is about the client's own fate, and that is why it belongs beside
//! them rather than in a test of its own — a window ending has two halves, and
//! each one used to take a process with it.
//!
//! Each case leaves its damage standing and then asks the compositor a
//! question it answers from its dispatch — the host asserts the other half,
//! that the desktop is still painting and that every client dropped on the way
//! was named with its pid.

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{exit, Command, Stdio};

use toyos::endow;
use toyos::AsHandle;
use toyos::{ipc, Connection};
use toyos_abi::syscall;
use toyos_abi::RawHandle;
use window::Window;

const SELF_PATH: &str = "/system/bin/test_rs_compositor_client_death";

/// The compositor connection, in the process that finishes the request its
/// creator did not live to send.
const RELAY_SOCKET: RawHandle = RawHandle(3);
/// The other end of the root's pipe, which closes when the creator has been
/// reaped. Nothing is ever read off it but the hang-up.
const RELAY_GO: RawHandle = RawHandle(4);

/// `timeout_nanos` for a wait with no clock (`syscall::inbox_submit`).
const FOREVER: u64 = u64::MAX;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("connect") => connect_and_go(),
        Some("finish") => finish(),
        Some(other) => panic!("unknown role {other:?}"),
        None => run(),
    }
}

fn run() {
    // **A creator that is gone before its window is asked for, with no race in
    // it.** `accept` names the process that called `connect`, and a connection
    // outlives that process — so the pid the compositor grants to here is one
    // the kernel has already forgotten.
    //
    // Racing a dying creator against the compositor's own dispatch is what
    // this used to do, and under a loaded host the compositor won all eight
    // heats and the run proved nothing. Instead the request is *completed by a
    // third process*: the creator hands its socket to a grandchild and exits,
    // this process reaps it — which is what takes the pid out of the process
    // table — and only then closes the pipe that releases the grandchild to
    // send the frame. Every step waits on the one before it.
    let mut creator = Command::new(SELF_PATH)
        .arg("connect")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| fail(&format!("[a reaped creator] spawn failed: {e}")));
    let go = creator.stdin.take().expect("the creator's stdin");
    let mut said = String::new();
    BufReader::new(creator.stdout.take().expect("the creator's stdout"))
        .read_line(&mut said)
        .unwrap_or_else(|e| fail(&format!("[a reaped creator] it never connected: {e}")));
    if !said.starts_with("connected") {
        fail(&format!("[a reaped creator] the creator said {said:?}"));
    }
    creator.wait().expect("reap the creator");
    // The reap is what makes the pid unknown; this is what tells the grandchild
    // the reap has happened.
    drop(go);
    probe("a creator reaped before its window");

    // A window is a connection promoted by its first frame, so a second
    // `MSG_CREATE_WINDOW` on one arrives with nothing to promote. The
    // compositor read that as its own bug.
    waiting("a second create on a live window", "its window");
    let doubled = Window::create(64, 64).expect("a window to send a second create on");
    write_handle(doubled.handle(), &create_frame(), "a second create");
    probe("a second create on a live window");

    // A commit with no copy begun: there is no region for it to name, so this
    // must cost the client its connection and nothing else.
    let commit = endow::service("compositor").expect("a connection to commit on");
    ipc::signal(commit.as_handle(), window::MSG_COPY_COMMIT).expect("send the commit");
    probe("a commit with no copy begun");

    // A copy no region is made for. The length decides how large a region the
    // compositor makes, so it is the compositor's to bound rather than the
    // client's to choose.
    let begin = endow::service("compositor").expect("a connection to begin on");
    begin
        .send(window::MSG_COPY_BEGIN, &window::ClipboardShmMsg { len: u32::MAX })
        .expect("send the begin");
    probe("a copy longer than any clipboard");

    // An inline clipboard one byte past what any client may inline. The
    // compositor keeps that one byte, so the frame is refusable here instead of
    // being stored as the prefix `ipc::FrameRx` would otherwise hand it.
    let over = window::MAX_INLINE_PAYLOAD + 1;
    let mut frame = vec![b'x'; 8 + over];
    frame[..4].copy_from_slice(&window::MSG_CLIPBOARD_SET.to_ne_bytes());
    frame[4..8].copy_from_slice(&(over as u32).to_ne_bytes());
    let conn = endow::service("compositor").expect("a connection to over-fill");
    write_handle(conn.as_handle(), &frame, "an over-long inline clipboard");
    probe("an over-long inline clipboard");

    // The other side of a window ending: the client has to be able to leave.
    // `MSG_DESTROY_WINDOW` makes the compositor drop the connection, after
    // which the handle is permanently read-ready at EOF — so a `poll_event` that
    // did not latch answered `Close` for as long as anybody kept asking, and a
    // client draining until `None` never got out. Two calls decide it, and
    // the second waits for nothing: a latched window answers `None` at once,
    // and an unlatched one reads the end of the stream at once.
    let what = "a window closed from the inside";
    waiting(what, "its window");
    let mut ending = Window::create(64, 64).expect("a window to close from the inside");
    ipc::signal(ending.handle(), window::MSG_DESTROY_WINDOW)
        .expect("ask the compositor to destroy this window");
    waiting(what, "the window's Close");
    loop {
        match ending.poll_event(FOREVER) {
            Some(window::Event::Close) => break,
            // A frame the compositor had already sent can arrive first. It is
            // not what this case is about, and skipping it is not a weakening:
            // what follows still has to be close and then nothing.
            Some(_) => {}
            None => fail(&format!("[{what}] the connection went and the window never said so")),
        }
    }
    waiting(what, "the latched poll answering None");
    if ending.poll_event(FOREVER).is_some() {
        fail(&format!(
            "[{what}] the poll after Close answered again — a client that drains until None \
             cannot leave"
        ));
    }
    probe(what);

    println!("compositor client death: 6 deaths survived, compositor still serving");
}

/// The creator: connect, hand the connection to a process that will outlive
/// this one, and go.
///
/// Nothing is sent here. The compositor's record of who this connection
/// belongs to is made at `connect`, and that is the only thing this role has
/// to establish before dying.
fn connect_and_go() {
    let conn = endow::service("compositor").expect("the compositor is not serving");
    // The kernel clones the handle into the child's table
    // (`loader::build_child_handles`), so the socket — and the pipes under it —
    // outlive this process.
    Command::new(SELF_PATH)
        .arg("finish")
        .inherit_handle(RELAY_SOCKET.0, conn.as_handle().0)
        .inherit_handle(RELAY_GO.0, 0)
        .spawn()
        .expect("spawn the process that finishes the request");
    println!("connected");
}

/// The grandchild: send the request its creator never sent, once that creator
/// has been reaped.
fn finish() {
    let mut byte = [0u8; 1];
    // The hang-up is the signal and the only signal: the root closes its end
    // after `wait` returns, and `wait` returning is the pid leaving the
    // process table.
    while let Ok(1) = syscall::read(RELAY_GO, &mut byte) {}
    write_handle(RELAY_SOCKET, &create_frame(), "finish");

    // **The answer is the non-vacuity witness, and it changed sides.** The
    // compositor used to say "the process behind it has exited" here, because
    // it granted the buffer to the pid `accept` reported and the kernel had
    // forgotten that pid. There is no pid and no grant: the buffer is a handle
    // sent over this connection, which is alive because this process holds it.
    // So the request is *served*, and the line that proves the compositor met
    // it is the answer rather than a refusal.
    let header = ipc::recv_header(RELAY_SOCKET).expect("the compositor answered");
    let what = if header.msg_type == window::MSG_WINDOW_CREATED { "a window" } else { "nothing" };
    // Stderr, because stdout is the pipe the root read one line off and let go
    // of: this process outlives the reader of its own stdout, and stderr is the
    // console both it and the compositor already share.
    eprintln!("a reaped creator's connection still got {what}");
}

/// A whole `MSG_CREATE_WINDOW` for a 64x64 window, header and payload.
fn create_frame() -> Vec<u8> {
    let payload_len = core::mem::size_of::<window::CreateWindowRequest>();
    let mut frame = vec![0u8; 8 + payload_len];
    frame[..4].copy_from_slice(&window::MSG_CREATE_WINDOW.to_ne_bytes());
    frame[4..8].copy_from_slice(&(payload_len as u32).to_ne_bytes());
    frame[8..12].copy_from_slice(&64u32.to_ne_bytes());
    frame[12..16].copy_from_slice(&64u32.to_ne_bytes());
    frame
}

/// Every write here fits in the pipe it goes into, so a blocking `write` can
/// only be the compositor's problem, never this binary's.
fn write_handle(handle: toyos_abi::RawHandle, bytes: &[u8], what: &str) {
    let mut offset = 0;
    while offset < bytes.len() {
        match syscall::write(handle, &bytes[offset..]) {
            Ok(n) => offset += n,
            Err(e) => fail(&format!("[{what}] write failed after {offset} bytes: {e:?}")),
        }
    }
}

/// Ask the compositor something it always answers from its dispatch, so an
/// answer proves the event loop reached the end of a pass rather than merely
/// that the process still exists.
fn probe(what: &str) {
    let conn: Connection = endow::service("compositor")
        .unwrap_or_else(|e| fail(&format!("[{what}] the compositor is not serving: {e:?}")));
    if let Err(e) = ipc::signal(conn.as_handle(), window::MSG_GET_RESOLUTION) {
        fail(&format!("[{what}] could not ask the compositor for its resolution: {e:?}"));
    }
    waiting(what, "the compositor's answer to a probe");
    let header = conn
        .recv_header()
        .unwrap_or_else(|e| fail(&format!("[{what}] the probe went unanswered: {e:?}")));
    if header.msg_type != window::MSG_RESOLUTION_CHANGED {
        fail(&format!("[{what}] the probe was answered with message type {}", header.msg_type));
    }
}

/// Said before each wait on the compositor: the line a missing event leaves
/// last.
fn waiting(what: &str, awaited: &str) {
    println!("compositor client death: [{what}] waiting for {awaited}");
}

fn fail(msg: &str) -> ! {
    eprintln!("compositor client death: {msg}");
    exit(1);
}
