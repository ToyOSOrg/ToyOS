//! The stop's claim on the console wire, and its holder's answer to it.
//!
//! The stop [`Handoff::ask`]s and then tries the wire; every holder lets the
//! wire go and then reads [`Handoff::asked`], and posts that it let go when it
//! was. Each side's write and its later read are split by a `SeqCst` fence, so
//! at least one of them sees the other's: the stop's try finds the wire free,
//! or the holder sees the ask and posts. Without them both can read stale, and the
//! stop waits out its whole budget on a wire nobody holds.
//! Compiled a second time by `kernel-loom`, whose `console_handoff` drives
//! both sides over the real `SleepLock`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{fence, AtomicBool, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{fence, AtomicBool, Ordering};

pub struct Handoff {
    asked: AtomicBool,
}

impl Handoff {
    /// Must stay `const`: the console's hand-off is a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { asked: AtomicBool::new(false) }
    }

    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { asked: AtomicBool::new(false) }
    }

    /// The stop's, once, before it first tries the wire.
    pub fn ask(&self) {
        self.asked.store(true, Ordering::Relaxed);
        fence(Ordering::SeqCst);
    }

    /// `klogd`'s, after it let the wire go, and between two writes it makes.
    pub fn asked(&self) -> bool {
        fence(Ordering::SeqCst);
        self.asked.load(Ordering::Relaxed)
    }
}
