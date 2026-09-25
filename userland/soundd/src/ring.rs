//! A bounded ring with one producer and one consumer, lock-free on both sides.
//!
//! soundd's threads hand each other work through it wherever one side is the
//! RT mix thread, which may never wait on the other: the control thread's
//! commands reach the mix thread through one (`command`), and every thread's
//! lines reach the one thread that writes them through one each (`say`).
//!
//! **One producer and one consumer is the caller's contract**, not the type's:
//! both ends take `&self`, and two threads pushing onto one ring race on the
//! same slot. Each user says at its site how it holds that.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

pub(crate) struct Spsc<T, const N: usize> {
    slots: UnsafeCell<[Option<T>; N]>,
    write_idx: AtomicU32,
    read_idx: AtomicU32,
}

// SAFETY: a slot is written only by the one producer before it publishes the
// write index, and taken only by the one consumer after it has read that index
// with acquire; the release/acquire pairs on the two indices order every slot
// access against the other side's.
unsafe impl<T: Send, const N: usize> Sync for Spsc<T, N> {}

impl<T, const N: usize> Spsc<T, N> {
    pub(crate) const fn new() -> Self {
        // A power of two, because the indices wrap mod 2^32 and a slot is the
        // index mod `N`.
        const { assert!(N.is_power_of_two() && N <= 1 << 31) };
        Self {
            slots: UnsafeCell::new([const { None }; N]),
            write_idx: AtomicU32::new(0),
            read_idx: AtomicU32::new(0),
        }
    }

    /// Hands the value back when the ring is full; what a full ring means is the
    /// caller's decision.
    #[must_use]
    pub(crate) fn try_push(&self, value: T) -> Result<(), T> {
        let w = self.write_idx.load(Ordering::Acquire);
        let r = self.read_idx.load(Ordering::Acquire);
        if w.wrapping_sub(r) >= N as u32 {
            return Err(value);
        }
        let idx = w as usize % N;
        unsafe { (*self.slots.get())[idx] = Some(value); }
        self.write_idx.store(w.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub(crate) fn pop(&self) -> Option<T> {
        let w = self.write_idx.load(Ordering::Acquire);
        let r = self.read_idx.load(Ordering::Acquire);
        if w == r {
            return None;
        }
        let idx = r as usize % N;
        let value = unsafe { (*self.slots.get())[idx].take() };
        self.read_idx.store(r.wrapping_add(1), Ordering::Release);
        value
    }
}
