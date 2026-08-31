//! The panic snapshot's atomic reader/writer state: `EMPTY` -> `WRITING` -> `READY`,
//! and `READY` -> `READING` once and for good.
//!
//! **`READING` is terminal.** Every writer transition here demands `EMPTY` or
//! `READY`, so once a fatal reader has entered, no capture, refresh or discard
//! runs on this snapshot again and the reader's borrow of it cannot be aliased.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU32, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU32, Ordering};

const EMPTY: u32 = 0;
const WRITING: u32 = 1;
const READY: u32 = 2;
const READING: u32 = 3;

pub struct CaptureAccess {
    state: AtomicU32,
}

impl CaptureAccess {
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { state: AtomicU32::new(EMPTY) }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { state: AtomicU32::new(EMPTY) }
    }

    pub fn begin_capture(&self) -> bool {
        #[cfg(not(feature = "loom"))]
        let changed = self.state.try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            EMPTY | READY => Some(WRITING),
            _ => None,
        });
        #[cfg(feature = "loom")]
        let changed = self.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            EMPTY | READY => Some(WRITING),
            _ => None,
        });
        changed.is_ok()
    }

    pub fn begin_refresh(&self) -> bool {
        self.state
            .compare_exchange(READY, WRITING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn publish(&self, captured: bool) {
        self.state.store(if captured { READY } else { EMPTY }, Ordering::Release);
    }

    pub fn read(&self) -> bool {
        #[cfg(not(feature = "loom"))]
        let changed = self.state.try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            READY | READING => Some(READING),
            _ => None,
        });
        #[cfg(feature = "loom")]
        let changed = self.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            READY | READING => Some(READING),
            _ => None,
        });
        changed.is_ok()
    }

    pub fn discard(&self) -> bool {
        #[cfg(not(feature = "loom"))]
        let changed = self.state.try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            EMPTY | READY => Some(EMPTY),
            _ => None,
        });
        #[cfg(feature = "loom")]
        let changed = self.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
            EMPTY | READY => Some(EMPTY),
            _ => None,
        });
        changed.is_ok()
    }
}
