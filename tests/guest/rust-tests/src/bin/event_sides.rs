//! An event's two sides on the real kernel: what `kernel::event`'s host tests
//! cannot show, which is the wake, the watch and the end across processes.
//!
//! **A parked wait is woken.** A thread parked in `SYS_HANDLE_WAIT` on a bell
//! is woken by a ring from another process, and by that process's end, which
//! closes its waker. Each happens only once the thread is parked, as
//! `pipe_other_end_gone` orders its leavings, and a thread left parked is a
//! hang the runner's ceiling reds.
//!
//! **A watch takes nothing.** An inbox watch answered readable, and a wait
//! with no timeout to spend, leave the ring pending for the take that follows.
//!
//! **An end is never a ring.** A bell rung once and then left by its last
//! waker answers the ring, and then only `Gone`; a waker whose bell has gone
//! answers `Gone` too.

use std::io::Read;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::poller::{wait_one, Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::wake::{self, Bell, Hungup, Waker};

#[path = "../roster.rs"]
mod roster;

const SELF_PATH: &str = "/system/bin/test_rs_event_sides";
const TOKEN: u64 = 7;
/// What the child is endowed its waker as.
const RING_LABEL: &str = "ring";
/// What the child is endowed its bell as.
const HEAR_LABEL: &str = "hear";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("child") => child(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

/// Rings its waker once its standard input says so, then waits until it ends,
/// holding its waker and its bell till it exits.
fn child() {
    let endowed = Endowments::get();
    let ring: Waker = endowed.take(RING_LABEL).expect("the waker this test endows");
    let _hear: Bell = endowed.take(HEAR_LABEL).expect("the bell this test endows");
    let mut stdin = std::io::stdin();
    let mut go = [0u8; 1];
    stdin.read_exact(&mut go).expect("the parent's word to ring");
    ring.ring().expect("the parent holds the bell");
    let mut rest = Vec::new();
    stdin.read_to_end(&mut rest).expect("the parent's end of standard input");
}

fn test() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");

    let (waker, bell) = wake::pair().expect("an event");
    assert!(!wait_one(&bell, READABLE, 1_000_000).expect("a wait on a bell"), "an unrung bell answered a wait");
    waker.wake();
    waker.wake();
    let poller = Poller::new(1);
    poller.watch(&bell, READABLE, TOKEN);
    let mut answers = Vec::new();
    poller.wait_answers(1, u64::MAX, |token, said| answers.push((token, said)));
    assert_eq!(answers, [(TOKEN, Ok(READABLE))], "a rung bell's watch");
    assert!(wait_one(&bell, READABLE, 0).expect("a look at a bell"), "a watch took the ring");
    assert_eq!(bell.try_take(), Ok(true), "two rings before a take are one, and a look took none");
    assert_eq!(bell.try_take(), Ok(false), "a take took the ring");
    println!("  two rings are one, and a watch and a look take nothing");

    let (parent_waker, parent_bell) = wake::pair().expect("the parent's event");
    let (child_waker, child_bell) = wake::pair().expect("the child's event");
    let mut child = Command::new(SELF_PATH)
        .arg("child")
        .endow(RING_LABEL, child_waker.into())
        .endow(HEAR_LABEL, parent_bell.into())
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    let mut stdin = child.stdin.take().expect("the child's standard input");
    woken(&cap, &child_bell, || std::io::Write::write_all(&mut stdin, b"g").expect("tell the child to ring"));
    assert_eq!(child_bell.try_take(), Ok(true), "the child's ring, still pending after the wait that saw it");
    println!("  a parked wait was woken by another process's ring");

    woken(&cap, &child_bell, || drop(stdin));
    assert!(child.wait().expect("wait the child").success(), "the child exited nonzero");
    assert_eq!(child_bell.try_take(), Err(Hungup), "a bell whose last waker went answered a ring nobody rang");
    assert!(wait_one(&child_bell, READABLE, 0).expect("a look at a bell"), "a bell with no waker left is not readable for good");
    assert_eq!(child_bell.try_take(), Err(Hungup), "a bell with no waker left answered a ring");
    assert_eq!(parent_waker.ring(), Err(Hungup), "a waker whose bell went answered a ring as heard");
    println!("event_sides: a ring wakes and coalesces, a watch takes nothing, and an end is never a ring");
}

/// A thread parked in one `SYS_HANDLE_WAIT` on `bell` is woken by `cause`,
/// which runs once the thread is parked.
fn woken(cap: &SysCap, bell: &Bell, cause: impl FnOnce()) {
    let entering = AtomicBool::new(false);
    let woke = thread::scope(|s| {
        let waiter = s.spawn(|| {
            entering.store(true, Ordering::Release);
            wait_one(bell, READABLE, u64::MAX).expect("a wait on a bell")
        });
        // The roster first: its syscall is the loop's preemption point.
        roster::await_true(|| {
            entering.load(Ordering::Acquire)
                && roster::my_threads(cap).iter().any(|&(is_thread, state)| is_thread && state == roster::BLOCKED)
        });
        cause();
        waiter.join().expect("the waiter thread panicked")
    });
    assert!(woke, "a wait with no timeout returned unready");
}
