//! A pipe end's `OTHER_END_GONE` watch: when it wakes, what it refuses, and
//! what it does not take for a leaving.
//!
//! **The wake.** A thread parks on a ring that holds one `OTHER_END_GONE`
//! watch and nothing else, and the other end's last holder goes: a reader is
//! told of its writer and a writer of its reader, with the ring empty and with
//! bytes left in it, and when the last holder was a process that ended. The
//! leaving waits for the park, as `inbox_cancel_wakes`'s close does, and a
//! waiter left parked is a hang the runner's ceiling reds. A watch made after
//! the leaving is answered by the submit that makes it.
//!
//! **The refusal.** A connection and a file have no other end, and a watch that
//! asks for one is refused whatever else it asks.
//!
//! **Not gone.** The other end has a holder while a duplicate of its handle is
//! held, while the handle sits unreceived in a connection's queue, and after
//! it moved to a child that still lives: the watch stays armed through each,
//! and is answered once that holder goes.

use std::io::Read;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::ipc::Connection;
use toyos::poller::{Poller, OTHER_END_GONE, READABLE};
use toyos::syscap::SysCap;
use toyos::{namespace, pipe_pair, port, AsHandle, OwnedHandle, Pipe};
use toyos_abi::syscall::{self, OpenFlags, SyscallError};
use toyos_abi::RawHandle;

#[path = "../roster.rs"]
mod roster;

const SELF_PATH: &str = "/system/bin/test_rs_pipe_other_end_gone";
const TOKEN: u64 = 7;
/// What the holding child is endowed the write end as. It never takes it: its
/// table holds it.
const HELD_LABEL: &str = "held";
const SERVICE: &str = "queue";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("hold") => hold(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

/// Holds what it was endowed until its standard input ends.
fn hold() {
    let mut rest = Vec::new();
    std::io::stdin().read_to_end(&mut rest).expect("the parent's end of standard input");
}

fn test() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");

    for unread in ["", "left in the ring"] {
        let (reader, writer) = pipe(unread);
        woken(&cap, &reader, || drop(writer));
        let (reader, writer) = pipe(unread);
        woken(&cap, &writer, || drop(reader));
    }
    println!("  a parked reader was told of its writer and a parked writer of its reader");

    let (client, server) = connection();
    refused(client.as_handle(), "a connection");
    let file = syscall::open(SELF_PATH.as_bytes(), OpenFlags::READ).expect("this binary's own file");
    refused(file, "a file");
    syscall::close(file);
    println!("  a connection and a file were refused the question");

    let (reader, writer) = pipe("");
    let second = syscall::dup(writer.as_handle()).expect("a second handle to the write end");
    drop(writer);
    held(&reader, "a duplicate of its handle is held");
    syscall::close(second);
    assert_eq!(at_last(&reader), Ok(OTHER_END_GONE), "the write end's last handle closed");

    let (reader, writer) = pipe("");
    client.send_handles([OwnedHandle::from(writer)]).expect("the write end into the connection's queue");
    held(&reader, "its handle sits unreceived in a connection's queue");
    let [received] = server.recv_handles_exact::<1>().expect("the handle the queue held");
    held(&reader, "its handle was received");
    syscall::close(received);
    assert_eq!(at_last(&reader), Ok(OTHER_END_GONE), "the received handle closed");

    let (reader, writer) = pipe("");
    let mut child = Command::new(SELF_PATH)
        .arg("hold")
        .endow(HELD_LABEL, writer.into_raw().0)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn the holder");
    let stdin = child.stdin.take().expect("the holder's standard input");
    held(&reader, "its handle moved to a child that still lives");
    woken(&cap, &reader, || drop(stdin));
    assert!(child.wait().expect("wait the holder").success(), "the holder exited nonzero");

    println!("pipe_other_end_gone: an end is told when its other end's last holder goes, and not before");
}

/// A fresh pipe with `unread` written into it.
fn pipe(unread: &str) -> (Pipe, Pipe) {
    let (reader, writer) = pipe_pair().expect("a pipe");
    if !unread.is_empty() {
        assert_eq!(writer.write_nonblock(unread.as_bytes()), Ok(unread.len()), "bytes into an empty pipe");
    }
    (reader, writer)
}

/// Both ends of one connection, made through a port of this process's own.
fn connection() -> (Connection, Connection) {
    let (acceptor, connector) = port::create().expect("a port");
    let names = namespace::build().add(SERVICE, &connector).finish().expect("a namespace");
    let client = names.open(SERVICE).expect("a connection to the port");
    let server = acceptor.accept().expect("the connection's other end");
    (client, server)
}

/// What one watch of `flags` on `handle` is answered by a submit that waits
/// until `min_complete` answers are in: `None` while the kernel keeps it armed.
fn watched(handle: RawHandle, flags: u32, min_complete: u32, timeout: u64) -> Option<Result<u32, SyscallError>> {
    let poller = Poller::new(1);
    poller.watch_raw(handle, flags, TOKEN);
    let mut answer: Option<Result<u32, SyscallError>> = None;
    poller.wait_answers(min_complete, timeout, |token, said| {
        assert_eq!(token, TOKEN, "an answer under a token nobody gave");
        assert!(answer.replace(said).is_none(), "one watch was answered twice");
    });
    answer
}

/// A watch for `end`'s other end leaving stays armed: the end has a holder.
fn held(end: &Pipe, because: &str) {
    assert_eq!(
        watched(end.as_handle(), OTHER_END_GONE, 0, 0),
        None,
        "an end was told its other end is gone while {because}"
    );
}

/// What a watch for `end`'s other end leaving is answered, however long that
/// takes: a release is finished by whichever CPU drains it.
fn at_last(end: &Pipe) -> Result<u32, SyscallError> {
    watched(end.as_handle(), OTHER_END_GONE, 1, u64::MAX).expect("a wait with no deadline returned with no answer")
}

/// `handle` names no pipe end, so a watch that asks for its other end is
/// refused, alone and beside a condition the handle does answer.
fn refused(handle: RawHandle, what: &str) {
    for flags in [OTHER_END_GONE, READABLE | OTHER_END_GONE] {
        assert_eq!(
            watched(handle, flags, 0, 0),
            Some(Err(SyscallError::NotSupported)),
            "a watch of {flags:#x} on {what}, which has no other end"
        );
    }
}

/// A thread parked on one `OTHER_END_GONE` watch of `end`, and on nothing
/// else, is woken by `leave`, which lets the other end's last holder go; and a
/// watch made after that is answered at once.
fn woken(cap: &SysCap, end: &Pipe, leave: impl FnOnce()) {
    let registered = AtomicBool::new(false);
    let answers = thread::scope(|s| {
        let waiter = s.spawn(|| {
            let poller = Poller::new(1);
            poller.watch(end, OTHER_END_GONE, TOKEN);
            let mut answers = Vec::new();
            // A non-blocking enter, so the watch is the kernel's before the
            // other end goes.
            poller.wait_answers(0, 0, |token, said| answers.push((token, said)));
            assert!(answers.is_empty(), "an end was told its other end is gone while it is held: {answers:?}");
            registered.store(true, Ordering::Release);
            poller.wait_answers(1, u64::MAX, |token, said| answers.push((token, said)));
            answers
        });
        // The roster first: its syscall is the loop's preemption point.
        roster::await_true(|| {
            roster::my_threads(cap).iter().any(|&(is_thread, state)| is_thread && state == roster::BLOCKED)
                && registered.load(Ordering::Acquire)
        });
        leave();
        waiter.join().expect("the waiter thread panicked")
    });
    assert_eq!(answers, [(TOKEN, Ok::<_, SyscallError>(OTHER_END_GONE))], "what the parked waiter was woken with");
    assert_eq!(
        watched(end.as_handle(), OTHER_END_GONE, 0, 0),
        Some(Ok(OTHER_END_GONE)),
        "a watch made after the other end's last holder left"
    );
}
