//! **One process parks 256 threads in `submit`, each its own ring's one
//! submitter, and a sibling thread wakes them all every round.**
//!
//! A ring's own watch sits behind a lock that masks interrupts, and a ring has
//! one submitter, so a thread's park and its wake are a walk over one
//! registration however many threads the process parks. This is the load
//! `mask_windows` reads the windows under.
//!
//! Each parked thread watches an event of its own on its own ring. The sibling
//! is this program's main thread: each round it signals every thread's event
//! once the kernel's roster shows all of them parked, and each thread takes its
//! event and parks again.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::wake::{Bell, Waker};
use toyos::AsHandle;
use toyos_abi::inbox::{RingHeader, Submission, COMPLETION_RING_OFF, OP_WATCH, READABLE, SUBMISSIONS_OFF, SUBMISSION_RING_OFF};
use toyos_abi::syscall;
use toyos_abi::RawHandle;

#[path = "../roster.rs"]
mod roster;

const PARKED: usize = 256;

/// Wakes each parked thread takes and parks again after.
const ROUNDS: u64 = 64;

/// A ring of one submission: the one watch its thread makes at a time.
const DEPTH: u32 = 1;

/// One thread's ring, as `toyos::poller` reaches it: one atomic word or one
/// whole entry at a time, since the kernel writes it too.
struct Ring {
    handle: RawHandle,
    base: *mut u8,
}

impl Ring {
    fn new() -> Self {
        // SAFETY: the page is read only through `Ring`, and its thread closes
        // the handle when it is done with both.
        let (handle, base) = unsafe { syscall::inbox_setup(DEPTH) }.expect("a ring");
        Self { handle, base }
    }

    fn word(&self, ring_off: u64, field_off: usize) -> &AtomicU32 {
        // SAFETY: inside the inbox page, 4-aligned; an atomic is sound over memory the kernel also writes.
        unsafe { AtomicU32::from_ptr(self.base.add(ring_off as usize + field_off) as *mut u32) }
    }

    /// Watch `bell` and park until it is answered, then take the answer back.
    fn park_on(&self, bell: &Bell, round: u64) {
        let tail = self.word(SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, tail));
        let at = tail.load(Ordering::Relaxed);
        let entry = Submission { op: OP_WATCH, handle: bell.as_handle().raw(), op_flags: READABLE, token: round, ..Submission::default() };
        // SAFETY: the one slot of the page's submission array, which only this thread writes.
        unsafe { (self.base.add(SUBMISSIONS_OFF as usize) as *mut Submission).write(entry) };
        tail.store(at.wrapping_add(1), Ordering::Release);
        let answered = syscall::inbox_submit(self.handle, 1, 1, u64::MAX).expect("a parked thread's submit");
        assert_eq!(answered, 1, "a parked thread returned with {answered} completions");
        self.word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, head)).fetch_add(1, Ordering::Release);
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: no type but this holds the number.
        unsafe { syscall::close(self.handle) };
    }
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let events: Vec<(Waker, Bell)> = (0..PARKED).map(|_| toyos::wake::pair().expect("an event")).collect();

    let parked_all = || {
        roster::my_threads(&cap)
            .iter()
            .filter(|&&(is_thread, state)| is_thread && state == roster::BLOCKED)
            .count()
            == PARKED
    };

    thread::scope(|s| {
        for (_, bell) in &events {
            s.spawn(move || {
                let ring = Ring::new();
                for round in 0..ROUNDS {
                    ring.park_on(bell, round);
                    assert!(bell.take(), "a thread was answered with no wake raised");
                }
            });
        }
        for _ in 0..ROUNDS {
            roster::await_true(parked_all);
            for (waker, _) in &events {
                waker.wake();
            }
        }
    });
    println!("ring_park_herd: {PARKED} threads parked in submit, each on its own ring, woken {ROUNDS} times");
}
