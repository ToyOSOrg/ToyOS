//! A cancelled `OP_WATCH` must wake the thread that is waiting for it.
//!
//! Two descriptors on one pipe, which is what an ordinary `dup`/`dup2` of
//! stdio leaves behind, and closing one of them is the whole stimulus. The
//! pipe keeps a reader either way, so `close_read`'s own wake path is not
//! involved and cannot mask the missing one.
//!
//! **The close waits for the park, and nothing here waits on a clock.** The
//! defect is only visible with the waiter parked — a close that lands first
//! leaves the completion sitting in the ring and the wait returns at once — so
//! the close is made once the kernel's roster says the waiter is blocked, and a
//! waiter the cancellation leaves parked is a hang the harness ceiling reds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos_abi::syscall;

#[path = "../roster.rs"]
mod roster;

const TOKEN: u64 = 7;

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let registered = AtomicBool::new(false);
    let pipe = syscall::pipe().expect("the pipe the waiter parks on");
    // The second descriptor. Closing this one is what `ops::close` answers polls for,
    // while `pipe.read` keeps the pipe's reader count above zero.
    let dup = syscall::dup(pipe.read).expect("dup the read end");

    let tokens = thread::scope(|s| {
        let waiter = s.spawn(|| {
            let poller = Poller::new(4);
            poller.watch_raw(pipe.read, READABLE, TOKEN);
            // A non-blocking enter, so the poll is registered in the kernel before
            // anything is closed. Without it the close could reach a ring with
            // nothing pending in it and cancel nothing at all, which is a
            // different test that would pass on a broken kernel.
            poller.wait(0, 0, |token| panic!("nothing is ready yet, got token {token}"));

            registered.store(true, Ordering::Release);
            let mut tokens = Vec::new();
            poller.wait(1, u64::MAX, |token| tokens.push(token));
            tokens
        });

        // The roster first: its syscall is the loop's preemption point.
        roster::await_true(|| {
            roster::my_threads(&cap).iter().any(|&(is_thread, state)| is_thread && state == roster::BLOCKED)
                && registered.load(Ordering::Acquire)
        });
        syscall::close(dup);
        waiter.join().expect("the waiter thread panicked")
    });
    assert_eq!(tokens, [TOKEN], "the wait returned with the wrong completions");
    println!("a cancelled poll woke its parked waiter");

    syscall::close(pipe.read);
    syscall::close(pipe.write);
}
