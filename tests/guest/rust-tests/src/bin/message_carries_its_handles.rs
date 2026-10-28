//! A connection carries handles only inside messages: the batch is pinned
//! just past its frame's bytes, so it arrives with them and with nothing else.
//!
//! - A frame's handles are taken with the frame.
//! - Handles with no bytes are refused whole: a poll-driven server sees
//!   nothing to read, and the sender still holds what it tried to send.
//! - A batch its frame did not announce stops the reader at it, and dies with
//!   the connection the reader then drops.

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, OTHER_END_GONE, READABLE};
use toyos::{namespace, pipe_pair, port, AsHandle, OwnedHandle, Pipe};
use toyos_abi::syscall::{self, SyscallError};

/// A failure ceiling only: an answer that never comes is the defect.
const CEILING_NANOS: u64 = 10_000_000_000;

fn main() {
    let (acceptor, connector) = port::create().expect("a port");
    let names = namespace::build().add("conn", &connector).finish().expect("a namespace");
    let pair = || {
        let client = names.open("conn").expect("connect");
        (client, acceptor.accept().expect("accept"))
    };

    // A frame's handles arrive with it.
    let (client, server) = pair();
    let (read_end, write_end) = pipe_pair().expect("a pipe");
    client.signal_with(7, [OwnedHandle::from(write_end)]).expect("a frame carrying a handle");
    let header = server.recv_header().expect("the frame");
    assert_eq!((header.msg_type, header.handles()), (7, 1), "the frame does not announce its handle");
    let [received] = server.recv_handles_exact::<1>(&header).expect("the handle, with its frame");
    drop(received);
    assert_eq!(gone(&read_end), Ok(OTHER_END_GONE), "the handle taken with its frame is not the one sent");
    println!("message_carries_its_handles: a frame's handle arrived with it");

    // Handles with no bytes are refused, and the server has nothing to read.
    let (stray_read, stray_write) = pipe_pair().expect("a pipe");
    assert_eq!(
        // SAFETY: no type holds the handles sent.
        unsafe { syscall::message_send(client.as_handle().raw(), &[], &[stray_write.as_handle().raw()], false) },
        Err(SyscallError::InvalidArgument),
        "handles were sent with no bytes"
    );
    assert!(!readable(&server, 0), "a refused send left the server something to read");
    assert_eq!(stray_write.write(b"x"), Ok(1), "a refused handle left its sender");
    drop(stray_write);
    assert_eq!(gone(&stray_read), Ok(OTHER_END_GONE), "the refused handle stayed anywhere but here");
    println!("message_carries_its_handles: handles alone were refused, and the server stayed idle");

    // A batch its frame did not announce stops the reader, and goes with the connection.
    let (client, server) = pair();
    let (lied_read, lied_write) = pipe_pair().expect("a pipe");
    let mut frame = [0u8; 12];
    frame[..4].copy_from_slice(&9u32.to_ne_bytes());
    // Given up before the send, which moves it: this process no longer holds the number.
    let lied_write = lied_write.into_raw();
    // SAFETY: no type holds the handles sent.
    unsafe { syscall::message_send(client.as_handle().raw(), &frame, &[lied_write], false) }
        .expect("a message whose frame announces no handle");
    let mut rx = FrameRx::<0>::new();
    assert!(readable(&server, CEILING_NANOS), "the message did not make the server readable");
    assert!(
        matches!(rx.pump(&server), RxStep::Frame { msg_type: 9, handles: 0, .. }),
        "the frame was not read as it was written"
    );
    assert!(matches!(rx.pump(&server), RxStep::Malformed), "the reader went past a batch no frame announced");
    drop(rx);
    drop(server);
    assert_eq!(gone(&lied_read), Ok(OTHER_END_GONE), "the unannounced batch outlived its connection");
    drop(client);
    println!("message_carries_its_handles: ok");
}

/// Whether `conn` answers readable within `timeout`.
fn readable(conn: &Connection, timeout: u64) -> bool {
    let poller = Poller::new(1);
    poller.watch(conn, READABLE, 1);
    let mut answered = false;
    poller.wait(u32::from(timeout != 0), timeout, |_| answered = true);
    answered
}

/// What a watch for `end`'s other end leaving is answered, within the ceiling.
fn gone(end: &Pipe) -> Result<u32, SyscallError> {
    let poller = Poller::new(1);
    poller.watch(&end, OTHER_END_GONE, 1);
    let mut answer = None;
    poller.wait_answers(1, CEILING_NANOS, |_, said| answer = Some(said));
    answer.expect("the other end never left")
}
