//! **One process parks 256 threads in `submit` on one ring, and a sibling
//! thread completes into it.**
//!
//! A ring's own watch sits behind a lock that masks interrupts, so every
//! completion notifies all 256 registrations with interrupts masked, and each
//! woken thread re-registers under that lock and parks again: the load
//! `issues/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`
//! names, which `mask_windows` reads the windows under.
//!
//! The sibling is this program's main thread. Each round it completes one
//! `OP_NOP` and takes the completion back, so the count the parked threads
//! wait for never reaches two, and it posts the next only once the kernel's
//! roster shows every one of them parked again. Two completions left standing
//! end the run.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::inbox::{RingHeader, Submission, COMPLETION_RING_OFF, OP_NOP, SUBMISSIONS_OFF, SUBMISSION_RING_OFF};
use toyos_abi::syscall;
use toyos_abi::RawHandle;

#[path = "../roster.rs"]
mod roster;

const PARKED: usize = 256;

/// Completions each parked thread is woken by and parks again after.
const ROUNDS: u64 = 64;

/// The fewest the ring takes: the sibling never has more than two in it.
const DEPTH: u32 = 2;

/// What every parked thread waits for, and what the sibling leaves only at the end.
const ENDING: u32 = 2;

/// The ring's page, as `toyos::poller` reaches it: one atomic word or one
/// whole entry at a time, since the kernel writes it too.
struct Ring {
    handle: RawHandle,
    base: *mut u8,
}

// SAFETY: the page stays mapped until the handle closes, after every thread is joined.
unsafe impl Sync for Ring {}

impl Ring {
    fn word(&self, ring_off: u64, field_off: usize) -> &AtomicU32 {
        // SAFETY: inside the inbox page, 4-aligned; an atomic is sound over memory the kernel also writes.
        unsafe { AtomicU32::from_ptr(self.base.add(ring_off as usize + field_off) as *mut u32) }
    }

    /// One `OP_NOP`, handed to the kernel, which completes it at once.
    fn complete_one(&self, token: u64) {
        let tail = self.word(SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, tail));
        let at = tail.load(Ordering::Relaxed);
        let entry = Submission { op: OP_NOP, token, ..Submission::default() };
        // SAFETY: the slot `at` masks to is inside the page's submission array, which only this thread writes.
        unsafe {
            (self.base.add(
                SUBMISSIONS_OFF as usize + (at & (DEPTH - 1)) as usize * core::mem::size_of::<Submission>(),
            ) as *mut Submission)
                .write(entry);
        }
        tail.store(at.wrapping_add(1), Ordering::Release);
        syscall::inbox_submit(self.handle, 1, 0, 0).expect("the sibling's submit");
    }

    /// Take the oldest completion back, so the count falls.
    fn consume_one(&self) {
        let head = self.word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, head));
        head.fetch_add(1, Ordering::Release);
    }
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    // SAFETY: the page is read only through `Ring`, and the handle closes after every thread is joined.
    let (handle, base) = unsafe { syscall::inbox_setup(DEPTH) }.expect("the ring");
    let ring = Ring { handle, base };

    let parked_all = || {
        roster::my_threads(&cap)
            .iter()
            .filter(|&&(is_thread, state)| is_thread && state == roster::BLOCKED)
            .count()
            == PARKED
    };

    thread::scope(|s| {
        for _ in 0..PARKED {
            s.spawn(|| {
                let answered = syscall::inbox_submit(ring.handle, 0, ENDING, u64::MAX)
                    .expect("a parked thread's submit");
                assert!(answered >= ENDING, "a parked thread returned with {answered} completions");
            });
        }
        roster::await_true(parked_all);
        for round in 0..ROUNDS {
            ring.complete_one(round);
            ring.consume_one();
            roster::await_true(parked_all);
        }
        for token in 0..u64::from(ENDING) {
            ring.complete_one(ROUNDS + token);
        }
    });
    syscall::close(handle);
    println!("ring_park_herd: {PARKED} threads parked in submit on one ring, woken by {ROUNDS} completions");
}
