//! The head of every line the loader says: how far into the loader it was
//! said, in milliseconds since `main`'s first statement, by the CPU's counter
//! at the rate the CPU states ([`crate::arch::counter_hz`]). No wall clock: a
//! line is placed against the loader's own start and nothing else.
//!
//! A CPU that states no rate gets lines with no head, and [`start`]'s caller
//! says so once.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

/// The counter at the loader's entry, and its rate; zero until [`start`].
static ENTRY: AtomicU64 = AtomicU64::new(0);
static HZ: AtomicU64 = AtomicU64::new(0);

/// Take `entry` as every stamp's zero, and return the rate stamps are
/// converted at, or `None` where the CPU states none.
pub fn start(entry: u64) -> Option<u64> {
    let hz = crate::arch::counter_hz();
    ENTRY.store(entry, Ordering::Relaxed);
    HZ.store(hz.unwrap_or(0), Ordering::Relaxed);
    hz
}

/// Now, as a line's head.
pub fn now() -> Stamp {
    let hz = HZ.load(Ordering::Relaxed);
    let ticks = crate::arch::counter().saturating_sub(ENTRY.load(Ordering::Relaxed));
    Stamp((hz != 0).then(|| (u128::from(ticks) * 1_000 / u128::from(hz)) as u64))
}

pub struct Stamp(Option<u64>);

impl fmt::Display for Stamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms:>5} ms "),
            None => Ok(()),
        }
    }
}
