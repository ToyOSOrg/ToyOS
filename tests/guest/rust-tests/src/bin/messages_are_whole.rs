//! A connection carries whole messages: each is taken whole, in the order it
//! was sent, with exactly the handles it carried, or refused with nothing of
//! it moved. What a host test of the queue cannot show is the kernel's own
//! boundary: the syscalls' refusals, the handles moving between tables and
//! the bounds as a sender meets them.
//!
//! - Three messages arrive in order, each with its own bytes and handles.
//! - A buffer a byte or a handle short is refused, and the message stays
//!   queued to be taken whole; one taken into two places fills them in
//!   order.
//! - Each bound (256 messages, 64 handles, 2 MiB of bytes) refuses the send
//!   past it with nothing moved, and room comes back as messages are taken.
//! - Plain reads and writes of a connection are refused.
//! - A receive after the peer has gone takes what it sent, then answers
//!   `Gone`; a send to a peer gone answers `Gone`.

use toyos::ipc::Connection;
use toyos::poller::OTHER_END_GONE;
use toyos::{namespace, pipe_pair, port, AsHandle, OwnedHandle, Pipe};
use toyos_abi::syscall::{
    self, Received, SyscallError, MAX_MESSAGE_LEN, MAX_QUEUED_HANDLES, MAX_QUEUED_MESSAGES, MAX_TRANSFER_HANDLES,
    PIPE_CAPACITY,
};
use toyos_abi::RawHandle;

/// A failure ceiling only: an answer that never comes is the defect.
const CEILING_NANOS: u64 = 10_000_000_000;

fn main() {
    let (acceptor, connector) = port::create().expect("a port");
    let names = namespace::build().add("conn", &connector).finish().expect("a namespace");
    let pair = || {
        let client = names.open("conn").expect("connect");
        (client, acceptor.accept().expect("accept"))
    };

    in_order_with_their_own_handles(pair());
    println!("messages_are_whole: three messages arrived in order, each with its own handles");
    a_short_buffer_leaves_it_queued(pair());
    println!("messages_are_whole: a short buffer took nothing, and the message came whole after");
    each_bound_refuses_the_send_past_it(pair());
    println!("messages_are_whole: each bound refused the send past it and moved nothing");
    bytes_alone_are_refused(pair());
    println!("messages_are_whole: plain reads and writes were refused");
    gone_after_what_was_sent(pair());
    println!("messages_are_whole: ok");
}

fn send(conn: &Connection, bytes: &[u8], handles: Vec<OwnedHandle>, nonblock: bool) -> Result<(), SyscallError> {
    let raw: Vec<RawHandle> = handles.iter().map(|h| h.as_handle().raw()).collect();
    // SAFETY: the handles are this process's; on success they are the peer's
    // and forgotten here, and on a refusal they are still here and close as
    // they drop.
    let sent = unsafe { syscall::message_send(conn.as_handle().raw(), bytes, &raw, nonblock) };
    if sent.is_ok() {
        for h in handles {
            // The number is the peer's now: given up, never closed here.
            let _ = h.into_raw();
        }
    }
    sent
}

/// The next message, its bytes and its handles, without waiting.
fn recv(conn: &Connection) -> Result<(Vec<u8>, Vec<OwnedHandle>), SyscallError> {
    let mut bytes = vec![0u8; MAX_MESSAGE_LEN];
    let mut handles = [RawHandle(0); MAX_TRANSFER_HANDLES];
    let got = syscall::message_recv(conn.as_handle().raw(), &mut [], &mut bytes, &mut handles, true)?;
    bytes.truncate(got.len);
    // SAFETY: the kernel installed these numbers in this process for this call.
    Ok((bytes, handles[..got.handles].iter().map(|&h| unsafe { OwnedHandle::from_raw(h) }).collect()))
}

/// A pipe end to send, and the end that sees it go.
fn token() -> (OwnedHandle, Pipe) {
    let (read, write) = pipe_pair().expect("a pipe");
    (OwnedHandle::from(write), read)
}

/// Whether `end`'s other end has gone, within the ceiling.
fn gone(end: &Pipe) -> bool {
    toyos::poller::wait_one(end, OTHER_END_GONE, CEILING_NANOS) == Ok(true)
}

fn in_order_with_their_own_handles((client, server): (Connection, Connection)) {
    let (a, a_seen) = token();
    let (c1, c1_seen) = token();
    let (c2, c2_seen) = token();
    send(&client, b"a", vec![a], false).expect("the first message");
    send(&client, b"bb", vec![], false).expect("the second message");
    send(&client, b"ccc", vec![c1, c2], false).expect("the third message");

    let (bytes, handles) = recv(&server).expect("the first message");
    assert_eq!((bytes.as_slice(), handles.len()), (&b"a"[..], 1), "the first message came cut or with another's handles");
    drop(handles);
    assert!(gone(&a_seen), "the first message's handle is not the one it carried");
    let (bytes, handles) = recv(&server).expect("the second message");
    assert_eq!((bytes.as_slice(), handles.len()), (&b"bb"[..], 0), "the second message came cut or with another's handles");
    let (bytes, mut handles) = recv(&server).expect("the third message");
    assert_eq!((bytes.as_slice(), handles.len()), (&b"ccc"[..], 2), "the third message came cut or with another's handles");
    drop(handles.remove(0));
    assert!(gone(&c1_seen), "the third message's handles came out of order");
    drop(handles);
    assert!(gone(&c2_seen), "the third message's second handle is not the one it carried");
    assert_eq!(recv(&server).err(), Some(SyscallError::WouldBlock), "a fourth message where three were sent");
}

fn a_short_buffer_leaves_it_queued((client, server): (Connection, Connection)) {
    let (h1, _) = token();
    let (h2, _) = token();
    send(&client, &[7; 8], vec![h1, h2], false).expect("a message of 8 bytes and two handles");
    let raw = server.as_handle().raw();
    let mut handles = [RawHandle(0); MAX_TRANSFER_HANDLES];
    assert_eq!(
        syscall::message_recv(raw, &mut [], &mut [0; 7], &mut handles, true),
        Err(SyscallError::InvalidArgument),
        "a buffer a byte short"
    );
    assert_eq!(
        syscall::message_recv(raw, &mut [], &mut [0; 8], &mut handles[..1], true),
        Err(SyscallError::InvalidArgument),
        "a handle buffer one short"
    );
    assert_eq!(
        syscall::message_recv(raw, &mut [0; 3], &mut [0; 4], &mut handles, true),
        Err(SyscallError::InvalidArgument),
        "two places a byte short together"
    );
    let mut bytes = [0u8; 8];
    let got = syscall::message_recv(raw, &mut [], &mut bytes, &mut handles, true).expect("the message, whole");
    assert_eq!((got, bytes), (Received { len: 8, handles: 2 }, [7; 8]), "the refused receives took part of it");
    // SAFETY: installed for this call.
    handles[..2].iter().for_each(|&h| drop(unsafe { OwnedHandle::from_raw(h) }));

    // Its first bytes fill the head, and the rest the body, in order.
    send(&client, &[1, 2, 3, 4, 5], vec![], false).expect("a message of five bytes");
    let (mut head, mut body) = ([0u8; 2], [0u8; 8]);
    let got = syscall::message_recv(raw, &mut head, &mut body, &mut [], true).expect("the message, split");
    assert_eq!((got.len, head, &body[..3]), (5, [1, 2], &[3, 4, 5][..]), "a split receive cut or reordered the message");
}

fn each_bound_refuses_the_send_past_it((client, server): (Connection, Connection)) {
    for i in 0..MAX_QUEUED_MESSAGES {
        send(&client, &[], vec![], true).unwrap_or_else(|e| panic!("message {i} refused {e:?} under the count bound"));
    }
    assert_eq!(send(&client, &[], vec![], true), Err(SyscallError::WouldBlock), "a message past the count bound");
    recv(&server).expect("one message taken");
    send(&client, &[], vec![], true).expect("room for one after one was taken");
    while recv(&server).is_ok() {}

    let mut seen = Vec::new();
    for _ in 0..MAX_QUEUED_HANDLES / MAX_TRANSFER_HANDLES {
        let batch = (0..MAX_TRANSFER_HANDLES).map(|_| token()).map(|(h, s)| {
            seen.push(s);
            h
        });
        send(&client, &[], batch.collect(), true).expect("handles under the bound");
    }
    let (one, one_seen) = token();
    assert_eq!(send(&client, &[], vec![one], true), Err(SyscallError::WouldBlock), "a handle past the handle bound");
    assert!(gone(&one_seen), "the refused handle was not given back to close");
    send(&client, &[], vec![], true).expect("a message without handles waits on no handle room");
    while recv(&server).is_ok() {}

    let full = vec![0u8; MAX_MESSAGE_LEN];
    for i in 0..PIPE_CAPACITY / MAX_MESSAGE_LEN {
        send(&client, &full, vec![], true).unwrap_or_else(|e| panic!("message {i} refused {e:?} under the byte bound"));
    }
    assert_eq!(send(&client, &[0], vec![], true), Err(SyscallError::WouldBlock), "a byte past the byte bound");
    recv(&server).expect("one full message taken");
    send(&client, &full, vec![], true).expect("room for one full message after one was taken");
    while recv(&server).is_ok() {}
}

fn bytes_alone_are_refused((client, server): (Connection, Connection)) {
    assert_eq!(syscall::write(client.as_handle().raw(), b"x"), Err(SyscallError::PermissionDenied), "a plain write");
    send(&client, b"y", vec![], false).expect("a message");
    assert_eq!(syscall::read(server.as_handle().raw(), &mut [0; 1]), Err(SyscallError::PermissionDenied), "a plain read");
    assert_eq!(recv(&server).map(|(b, _)| b), Ok(b"y".to_vec()), "the refused read took the message");
}

fn gone_after_what_was_sent((client, server): (Connection, Connection)) {
    send(&client, b"last", vec![], false).expect("a message before leaving");
    drop(client);
    assert_eq!(recv(&server).map(|(b, _)| b), Ok(b"last".to_vec()), "the message sent before leaving");
    assert_eq!(recv(&server).err(), Some(SyscallError::Gone), "after the last message, the leaving");
    assert_eq!(send(&server, b"x", vec![], false), Err(SyscallError::Gone), "a send to a peer gone");
}
