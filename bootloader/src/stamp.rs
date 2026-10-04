//! The head of every line the loader says: how far into the loader it was
//! said, in milliseconds since `main`'s first statement, by the CPU's counter
//! at the rate the CPU states ([`crate::arch::counter_hz`]). No wall clock: a
//! line is placed against the loader's own start and nothing else.
//!
//! A CPU that states no rate gets lines with no head, and [`start`]'s caller
//! says so once.
//!
//! [`say`] also counts what its lines cost, in counter ticks, split between the
//! console and `loader.log`: [`cost`] is the two sums.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

/// The counter at the loader's entry, and its rate; zero until [`start`].
static ENTRY: AtomicU64 = AtomicU64::new(0);
static HZ: AtomicU64 = AtomicU64::new(0);

static LINES: AtomicU64 = AtomicU64::new(0);
static CONSOLE_TICKS: AtomicU64 = AtomicU64::new(0);
static FILE_TICKS: AtomicU64 = AtomicU64::new(0);

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
    let ticks = crate::arch::counter()
        .checked_sub(ENTRY.load(Ordering::Relaxed))
        .expect("the counter ran backwards since the loader's entry");
    Stamp((hz != 0).then(|| (u128::from(ticks) * 1_000 / u128::from(hz)) as u64))
}

/// One line, stamped, to the console and then to `loader.log`.
pub fn say(args: fmt::Arguments) {
    let stamp = now();
    let before = crate::arch::counter();
    uefi_services::println!("{stamp}{args}");
    let between = crate::arch::counter();
    crate::loaderlog::line(format_args!("{stamp}{args}"));
    let after = crate::arch::counter();
    LINES.fetch_add(1, Ordering::Relaxed);
    CONSOLE_TICKS.fetch_add(between - before, Ordering::Relaxed);
    FILE_TICKS.fetch_add(after - between, Ordering::Relaxed);
}

/// The lines [`say`] has said, and the counter ticks they spent on the console
/// and in `loader.log`.
pub fn cost() -> (u64, u64, u64) {
    (LINES.load(Ordering::Relaxed), CONSOLE_TICKS.load(Ordering::Relaxed), FILE_TICKS.load(Ordering::Relaxed))
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
