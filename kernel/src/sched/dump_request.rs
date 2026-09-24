//! Ctrl+Alt+D's request and the report it asks for, as one word: what is pending, and whether a report runs.
//! Every transition is one atomic update of that word, so a request is announced once and taken once, no
//! report begins inside another, and a request filed during a report is taken by that report's end.
//! No `crate::` references: `kernel-loom` compiles this file directly under `feature = "loom"`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU8, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU8, Ordering};

/// The report's handoff: what one report wrote, the next to take the word reads. `dump-report-relaxed` makes
/// it `Relaxed` so `kernel-loom` can prove the edge is load-bearing; no kernel build turns it on.
#[cfg(not(feature = "dump-report-relaxed"))]
const HANDOFF: (Ordering, Ordering) = (Ordering::AcqRel, Ordering::Acquire);
#[cfg(feature = "dump-report-relaxed")]
const HANDOFF: (Ordering, Ordering) = (Ordering::Relaxed, Ordering::Relaxed);

/// The bits that say what is pending.
const PENDING: u8 = 0b011;
const NONE: u8 = 0;
/// Asked for, and no pass has said it left it.
const REQUESTED: u8 = 1;
/// Asked for, and a pass that may not serve it has said so.
const ANNOUNCED: u8 = 2;
/// A report runs; whoever set it owns the report's state until it clears it.
const REPORTING: u8 = 0b100;

/// What a pass that may not serve the request found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Left {
    Nothing,
    /// Pending, and this pass is the first to leave it: the one that says so.
    First,
    /// Pending, and an earlier pass has said so.
    Again,
}

pub struct DumpRequest {
    word: AtomicU8,
}

impl DumpRequest {
    /// Must stay `const`: the kernel seeds it as a `static`.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { word: AtomicU8::new(NONE) }
    }

    // No `Default`: `Default::default` can't be `const`, unlike the arm above.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { word: AtomicU8::new(NONE) }
    }

    /// One atomic update: `next` answers the word that replaces the one it is shown, or refuses it. The
    /// compare-exchange judges the word as it is, so a refusal alone can rest on a stale read.
    fn update(&self, set: Ordering, read: Ordering, next: impl Fn(u8) -> Option<u8>) -> Result<u8, u8> {
        let mut word = self.word.load(read);
        loop {
            let Some(new) = next(word) else { return Err(word) };
            match self.word.compare_exchange_weak(word, new, set, read) {
                Ok(was) => return Ok(was),
                Err(now) => word = now,
            }
        }
    }

    /// Asked; asking while one is pending is that same request.
    pub fn file(&self) {
        let _ = self.update(Ordering::Relaxed, Ordering::Relaxed, |word| {
            (word & PENDING == NONE).then_some(word | REQUESTED)
        });
    }

    /// Ask and begin the report in one update, unless a report runs: no pass can take the request between
    /// its filing and its taking, so the asker owns the report it asked for, and it serves anything pending.
    #[cfg(any(feature = "boot-actuators", feature = "loom"))]
    pub fn file_and_take(&self) -> bool {
        self.update(HANDOFF.0, HANDOFF.1, |word| (word & REPORTING == 0).then_some(REPORTING)).is_ok()
    }

    /// Whether a request is pending. A read, for the pass that has nothing to take.
    pub fn pending(&self) -> bool {
        self.word.load(Ordering::Relaxed) & PENDING != NONE
    }

    /// Leave a pending request for a pass that may serve it.
    pub fn leave(&self) -> Left {
        let announced = self.update(Ordering::Relaxed, Ordering::Relaxed, |word| {
            (word & PENDING == REQUESTED).then_some((word & !PENDING) | ANNOUNCED)
        });
        match announced {
            Ok(_) => Left::First,
            Err(word) if word & PENDING == NONE => Left::Nothing,
            Err(_) => Left::Again,
        }
    }

    /// Take a pending request and begin its report, unless a report runs: that report's end takes it.
    pub fn take(&self) -> bool {
        self.update(HANDOFF.0, HANDOFF.1, |word| {
            (word & PENDING != NONE && word & REPORTING == 0).then_some(REPORTING)
        })
        .is_ok()
    }

    /// The report ended. `true` if a request was filed during it: this took it, and the caller reports again.
    pub fn end_report(&self) -> bool {
        let ended = self.update(HANDOFF.0, HANDOFF.1, |word| {
            Some(if word & PENDING == NONE { NONE } else { REPORTING })
        });
        let (Ok(was) | Err(was)) = ended;
        was & PENDING != NONE
    }
}
