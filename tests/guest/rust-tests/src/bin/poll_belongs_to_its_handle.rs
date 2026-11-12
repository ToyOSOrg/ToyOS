//! A watch belongs to the handle it was made through, and a ring to its one
//! submitter.
//!
//! - A duplicate of a pipe's read end, or of an event's wait side, closed
//!   under a watch on the original answers nothing: the watch answers the
//!   readiness that comes after.
//! - The watched handle's own close answers `NotFound`, and wakes the
//!   submitter parked on it.
//! - A child watched through a duplicate is answered `NotFound` at that
//!   duplicate's close, while the child lives.
//! - A watched handle sent away is answered `NotFound`.
//! - A second submit while one is parked on the ring is refused `Busy`.
//! - A ring closed under an unbounded submit returns that submit `NotFound`.
//!
//! **A close waits for the park, and nothing here waits on a clock**: a
//! close that lands before the waiter parks leaves its answer in the ring,
//! and the wait returns without ever having parked. So each close is made once
//! the kernel's roster says the waiter is blocked, and a waiter the close
//! leaves parked is a hang the harness ceiling reds.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::ipc::Connection;
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::{namespace, port, AsHandle, OwnedHandle};
use toyos_abi::inbox::{RingHeader, Submission, OP_WATCH, SUBMISSIONS_OFF, SUBMISSION_RING_OFF};
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

#[path = "../roster.rs"]
mod roster;

const SELF_PATH: &str = "/system/bin/test_rs_poll_belongs_to_its_handle";

const TOKEN: u64 = 7;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        // Lives until its parent closes its stdin.
        let _ = std::io::stdin().read(&mut [0u8; 1]);
        return;
    }
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows every binary a system capability");
    a_duplicates_close_answers_nothing();
    println!("poll_belongs_to_its_handle: a duplicate's close left the watch standing");
    the_watched_handles_close_answers_its_parked_submitter(&cap);
    println!("poll_belongs_to_its_handle: the watched handle's close answered NotFound");
    a_child_watched_through_a_duplicate_is_answered_at_its_close();
    println!("poll_belongs_to_its_handle: the duplicate's close answered NotFound while the child lives");
    a_watched_handle_sent_away_is_answered();
    println!("poll_belongs_to_its_handle: a handle sent away answered NotFound");
    a_second_submit_is_refused_and_a_closed_ring_returns_its_submit(&cap);
    println!("poll_belongs_to_its_handle: ok");
}

/// Every answer one wait for at least one returns.
fn answers(poller: &Poller) -> Vec<(u64, Result<u32, SyscallError>)> {
    let mut got = Vec::new();
    poller.wait_answers(1, u64::MAX, |token, answer| got.push((token, answer)));
    got
}

/// Register `poller`'s queued watches with the kernel, answering nothing.
fn registered(poller: &Poller) {
    poller.wait_answers(0, 0, |token, answer| panic!("nothing is ready yet: {token} answered {answer:?}"));
}

fn a_duplicates_close_answers_nothing() {
    let poller = Poller::new(2);

    let (read, write) = toyos::pipe_pair().expect("a pipe");
    let dup = read.as_handle().try_clone_to_owned().expect("a duplicate of the read end");
    poller.watch(&read, READABLE, TOKEN);
    registered(&poller);
    drop(dup);
    write.write(b"x").expect("a byte for the read end");
    assert_eq!(answers(&poller), [(TOKEN, Ok(READABLE))], "a pipe end's watch was ended by its duplicate's close");

    let (signal, wait) = toyos::wake::pair().expect("an event");
    let dup = wait.as_handle().try_clone_to_owned().expect("a duplicate of the wait side");
    poller.watch(&wait, READABLE, TOKEN + 1);
    registered(&poller);
    drop(dup);
    signal.wake();
    assert_eq!(answers(&poller), [(TOKEN + 1, Ok(READABLE))], "an event's watch was ended by its duplicate's close");
}

fn the_watched_handles_close_answers_its_parked_submitter(cap: &SysCap) {
    let (read, _write) = toyos::pipe_pair().expect("a pipe no byte enters");
    let raw = read.into_raw();
    let registered_once = AtomicBool::new(false);
    let answered = thread::scope(|s| {
        let waiter = s.spawn(|| {
            let poller = Poller::new(1);
            // SAFETY: the watch names the number alone; the main thread closes it.
            poller.watch(&unsafe { toyos::BorrowedHandle::borrow_raw(raw) }, READABLE, TOKEN);
            registered(&poller);
            registered_once.store(true, Ordering::Release);
            answers(&poller)
        });
        await_parked(cap, &registered_once);
        // SAFETY: this process's own pipe end, which nothing uses after this.
        unsafe { syscall::close(raw) };
        waiter.join().expect("the waiter")
    });
    assert_eq!(answered, [(TOKEN, Err(SyscallError::NotFound))], "the watched handle's own close");
}

fn a_child_watched_through_a_duplicate_is_answered_at_its_close() {
    let mut child = Command::new(SELF_PATH).arg("child").stdin(Stdio::piped()).spawn().expect("a child that waits");
    let dup = AsHandle::as_handle(&child).try_clone_to_owned().expect("a duplicate of the child's handle");
    let poller = Poller::new(1);
    poller.watch(&dup, READABLE, TOKEN);
    registered(&poller);
    drop(dup);
    assert_eq!(answers(&poller), [(TOKEN, Err(SyscallError::NotFound))], "a child watched through a closed duplicate");
    assert!(child.try_wait().expect("the child's state").is_none(), "the child ended, so its end may have answered");
    drop(child.stdin.take());
    assert!(child.wait().expect("the child's end").success());
}

fn a_watched_handle_sent_away_is_answered() {
    let (acceptor, connector) = port::create().expect("a port");
    let names = namespace::build().add("conn", &connector).finish().expect("a namespace");
    let client: Connection = names.open("conn").expect("a connection");
    let server = acceptor.accept().expect("its other end");
    let (read, _write) = toyos::pipe_pair().expect("a pipe no byte enters");
    let poller = Poller::new(1);
    poller.watch(&read, READABLE, TOKEN);
    registered(&poller);
    client.signal_with(1, [OwnedHandle::from(read)]).expect("the read end, sent");
    assert_eq!(answers(&poller), [(TOKEN, Err(SyscallError::NotFound))], "a watched handle sent away");
    drop(server);
}

/// An inbox of one submission and a watch on a pipe no byte enters, queued.
fn ring_watching(read: RawHandle) -> RawHandle {
    // SAFETY: the page is this ring's, mapped until its handle closes, and it
    // is reached only as the kernel also writes it: one atomic header word and
    // one whole submission.
    unsafe {
        let (inbox, base) = syscall::inbox_setup(1).expect("an inbox");
        let header = base.add(SUBMISSION_RING_OFF as usize).cast::<RingHeader>();
        let tail = (*header).tail.load(Ordering::Acquire);
        let submission = Submission { op: OP_WATCH, handle: read, op_flags: READABLE, token: TOKEN, ..Submission::default() };
        base.add(SUBMISSIONS_OFF as usize).cast::<Submission>().write(submission);
        (*header).tail.store(tail.wrapping_add(1), Ordering::Release);
        inbox
    }
}

fn a_second_submit_is_refused_and_a_closed_ring_returns_its_submit(cap: &SysCap) {
    let (read, write) = toyos::pipe_pair().expect("a pipe");
    let inbox = ring_watching(read.as_handle().raw());
    let parked = AtomicBool::new(false);
    let returned = thread::scope(|s| {
        let waiter = s.spawn(|| {
            parked.store(true, Ordering::Release);
            syscall::inbox_submit(inbox, 1, 1, u64::MAX)
        });
        await_parked(cap, &parked);
        assert_eq!(syscall::inbox_submit(inbox, 0, 0, 0), Err(SyscallError::Busy), "a second submitter");
        write.write(b"x").expect("a byte for the watch");
        waiter.join().expect("the waiter")
    });
    assert_eq!(returned, Ok(1), "the first submitter was answered");
    // SAFETY: the ring's one handle, which nothing else answers for.
    unsafe { syscall::close(inbox) };

    let (idle, _write) = toyos::pipe_pair().expect("a pipe no byte enters");
    let inbox = ring_watching(idle.as_handle().raw());
    let parked = AtomicBool::new(false);
    let returned = thread::scope(|s| {
        let waiter = s.spawn(|| {
            parked.store(true, Ordering::Release);
            syscall::inbox_submit(inbox, 1, 1, u64::MAX)
        });
        await_parked(cap, &parked);
        // SAFETY: the ring's one handle, which nothing else answers for.
        unsafe { syscall::close(inbox) };
        waiter.join().expect("the waiter")
    });
    assert_eq!(returned, Err(SyscallError::NotFound), "a submit parked on a ring closed under it");
}

/// Once `ready` is set and the roster shows one of this process's threads
/// blocked. The roster first: its syscall is the loop's preemption point.
fn await_parked(cap: &SysCap, ready: &AtomicBool) {
    roster::await_true(|| {
        roster::my_threads(cap).iter().any(|&(is_thread, state)| is_thread && state == roster::BLOCKED)
            && ready.load(Ordering::Acquire)
    });
}
