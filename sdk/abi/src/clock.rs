//! The monotonic clock a process reads without a syscall, until the kernel
//! demotes the counter it reads.
//!
//! The clock is a straight [`Line`] through the counter: from a counter
//! reading, the nanoseconds since the counter's zero, and the rate after it.
//! The kernel lays the line out on one page before the first process exists,
//! maps that page read-only at [`CLOCK_PAGE`] in every address space, and
//! replaces the line when it learns the counter's rate better, starting the
//! new one where the old one stands, so the clock never steps. [`page`] reads
//! the line in force through the page's [`latch`], which the kernel's own
//! clock reads too, so a stamp taken here and one taken by the kernel's log
//! order against each other until the kernel demotes the page.
//!
//! **A demoted page sends every reading to the kernel.** The kernel demotes
//! the page when it stops trusting the counter ([`ClockPage::demoted`]);
//! [`nanos_since_boot`] and [`stamp_ns`] then ask
//! [`crate::syscall::clock_now`], which reads the reference instead. The line
//! stays, for a recorded counter reading.
//!
//! **A reading takes the counter inside the latch's read section**, and the
//! page's demoted word after the counter, so a line replaced or a demotion
//! made while the reader was preempted sends it round again or to the kernel.
//!
//! **Every word of the page is atomic.** The kernel writes it while processes
//! read it, so it is reached as atomics and never as plain memory.

mod latch;

use core::sync::atomic::{AtomicU64, Ordering};

pub use latch::Latch;

/// Where every address space finds the page: below the lowest address an
/// allocation or a placed `mmap` may take, so nothing a process maps can land
/// on it, and 2 MiB-aligned because that is the page the kernel maps.
pub const CLOCK_PAGE: u64 = 0x1_0000_0000;

/// What the page opens with, so a reader that finds anything else refuses
/// rather than computes a time out of it.
pub const CLOCK_MAGIC: u64 = 0x544F_594F_434C_4B33; // "TOYOCLK3"

/// The binary point of [`Line::mult`]: nanoseconds per tick in 32.32 fixed
/// point, which keeps a 5 GHz counter's rate to a part in 10⁹ and a 1 Hz
/// counter's tick representable.
pub const MULT_SHIFT: u32 = 32;

/// One straight piece of the clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// The counter reading the line starts at.
    pub counter: u64,
    /// The clock at `counter`: nanoseconds since the counter's zero.
    pub stamp: u64,
    /// Nanoseconds per counter tick, times 2^[`MULT_SHIFT`].
    pub mult: u64,
}

impl Line {
    /// The [`Line::mult`] of a counter at `hz`, rounded to nearest.
    pub const fn mult_of_hz(hz: u64) -> u64 {
        let ns = 1_000_000_000u128 << MULT_SHIFT;
        ((ns + hz as u128 / 2) / hz as u128) as u64
    }

    /// The rate [`Line::mult`] stands for, in millihertz.
    pub const fn millihertz(&self) -> u64 {
        ((1_000_000_000_000u128 << MULT_SHIFT) / self.mult as u128) as u64
    }

    /// `ticks` of the counter, in nanoseconds; saturating, which no count a
    /// machine reaches in a century touches.
    pub const fn nanos(&self, ticks: u64) -> u64 {
        let nanos = (ticks as u128 * self.mult as u128) >> MULT_SHIFT;
        if nanos > u64::MAX as u128 {
            u64::MAX
        } else {
            nanos as u64
        }
    }

    /// The counter ticks `nanos` takes, rounded down.
    pub const fn ticks(&self, nanos: u64) -> u64 {
        (((nanos as u128) << MULT_SHIFT) / self.mult as u128) as u64
    }

    /// The clock at `counter`. A reading before the line's start is carried
    /// back along it, which is where a record stamped before the kernel last
    /// replaced the line lands, and a CPU whose counter trails reads behind;
    /// nothing reads before the counter's zero.
    pub const fn stamp_of(&self, counter: u64) -> u64 {
        if counter >= self.counter {
            self.stamp.saturating_add(self.nanos(counter - self.counter))
        } else {
            self.stamp.saturating_sub(self.nanos(self.counter - counter))
        }
    }

    const fn words(&self) -> [u64; 3] {
        [self.counter, self.stamp, self.mult]
    }

    const fn of_words(words: [u64; 3]) -> Self {
        Self { counter: words[0], stamp: words[1], mult: words[2] }
    }
}

/// The clock as one reading of the page found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// The line in force.
    pub line: Line,
    /// The clock at boot: what [`Clock::nanos_since_boot`] counts from.
    pub stamp_at_boot: u64,
}

impl Clock {
    /// A counter reading as a log line's time: nanoseconds since the counter's
    /// zero.
    pub const fn stamp_of(&self, counter: u64) -> u64 {
        self.line.stamp_of(counter)
    }

    /// A counter reading as nanoseconds since boot; a counter that reads
    /// before boot reads as boot.
    pub const fn nanos_since_boot(&self, counter: u64) -> u64 {
        self.stamp_of(counter).saturating_sub(self.stamp_at_boot)
    }
}

/// The page's layout.
#[repr(C)]
pub struct ClockPage {
    magic: AtomicU64,
    stamp_at_boot: AtomicU64,
    demoted: AtomicU64,
    line: Latch<3>,
}

impl ClockPage {
    /// Lay the page out: the kernel's, once, before any address space maps it.
    pub fn init(&self, stamp_at_boot: u64, line: Line) {
        self.stamp_at_boot.store(stamp_at_boot, Ordering::Relaxed);
        self.line.publish(line.words());
        self.magic.store(CLOCK_MAGIC, Ordering::Release);
    }

    /// Replace the line: the kernel's, one writer at a time.
    pub fn publish(&self, line: Line) {
        self.line.publish(line.words());
    }

    /// Send every reading to the kernel from now on: the kernel's, once.
    pub fn demote(&self) {
        self.demoted.store(1, Ordering::Release);
    }

    /// Whether a reading of now asks the kernel rather than the line.
    pub fn demoted(&self) -> bool {
        self.demoted.load(Ordering::Acquire) != 0
    }

    /// The clock, a counter reading `counter` took inside the line's read
    /// section, and whether the page was demoted after it; `None` on a page
    /// that carries none.
    pub fn read_now(&self, counter: impl Fn() -> u64) -> Option<(Clock, u64, bool)> {
        if self.magic.load(Ordering::Acquire) != CLOCK_MAGIC {
            return None;
        }
        let (words, (now, demoted)) = self.line.read_with(|| {
            let now = counter();
            (now, self.demoted())
        });
        Some((Clock { line: Line::of_words(words), stamp_at_boot: self.stamp_at_boot.load(Ordering::Relaxed) }, now, demoted))
    }

    /// The clock now, or `None` on a page that carries none.
    pub fn read(&self) -> Option<Clock> {
        if self.magic.load(Ordering::Acquire) != CLOCK_MAGIC {
            return None;
        }
        let line = Line::of_words(self.line.read());
        Some(Clock { line, stamp_at_boot: self.stamp_at_boot.load(Ordering::Relaxed) })
    }
}

/// Nanoseconds since boot, off this process's clock page, or the kernel's
/// clock where the page is demoted.
///
/// # Panics
/// As [`page`].
pub fn nanos_since_boot() -> u64 {
    let (clock, now, demoted) = read_now();
    if demoted {
        return crate::syscall::clock_now().saturating_sub(clock.stamp_at_boot);
    }
    clock.nanos_since_boot(now)
}

/// Now as a log line's time: nanoseconds since the counter's zero, the kernel
/// log's own `clock::stamp`, which every line of the log carries
/// (`log::LogRecord::at_ns`); the kernel's clock where the page is demoted.
///
/// # Panics
/// As [`page`].
pub fn stamp_ns() -> u64 {
    let (clock, now, demoted) = read_now();
    if demoted {
        return crate::syscall::clock_now();
    }
    clock.stamp_of(now)
}

fn read_now() -> (Clock, u64, bool) {
    mapped()
        .read_now(crate::arch::counter)
        .unwrap_or_else(|| panic!("the clock page at {CLOCK_PAGE:#x} carries no clock"))
}

/// The clock, off this process's page: its line, for a counter reading.
///
/// # Panics
/// On a page that does not carry [`CLOCK_MAGIC`]: every address space the
/// kernel builds maps one, so its absence is a kernel that broke the ABI.
pub fn page() -> Clock {
    read(mapped())
}

fn mapped() -> &'static ClockPage {
    // SAFETY: the kernel maps the page read-only at `CLOCK_PAGE` in every
    // address space before the first instruction runs and never unmaps it;
    // every field is an atomic, so a shared reference reads it as atomics.
    unsafe { &*(CLOCK_PAGE as *const ClockPage) }
}

fn read(page: &ClockPage) -> Clock {
    page.read().unwrap_or_else(|| panic!("the clock page at {CLOCK_PAGE:#x} carries no clock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 GHz counter: one tick a nanosecond.
    const GHZ: u64 = 1 << MULT_SHIFT;

    #[test]
    fn a_line_counts_from_its_start() {
        let line = Line { counter: 100, stamp: 5_000, mult: GHZ };
        assert_eq!(line.stamp_of(1_100), 6_000);
        assert_eq!(line.stamp_of(100), 5_000);
        // Carried back before the start, and never before the counter's zero.
        assert_eq!(line.stamp_of(40), 4_940);
        assert_eq!(Line { counter: 10_000, stamp: 5, mult: GHZ }.stamp_of(0), 0);
        // Saturating at a counter no machine reaches.
        assert_eq!(line.stamp_of(u64::MAX), u64::MAX);
    }

    #[test]
    fn boot_reads_zero_and_before_boot_reads_boot() {
        let clock = Clock { line: Line { counter: 1_000, stamp: 2_000_000_000, mult: GHZ }, stamp_at_boot: 2_000_000_000 };
        assert_eq!(clock.nanos_since_boot(1_000), 0);
        assert_eq!(clock.nanos_since_boot(1_500), 500);
        assert_eq!(clock.nanos_since_boot(999), 0);
    }

    /// The rate round-trips through `mult` to a part in 10⁹ for every counter
    /// a machine has, the generic timer's 24 MHz to a 5 GHz TSC, and a second
    /// of ticks is a second to the nanosecond.
    #[test]
    fn mult_keeps_the_rate() {
        for hz in [24_000_000, 1_796_624_812, 2_419_200_000, 5_000_000_000] {
            let line = Line { counter: 0, stamp: 0, mult: Line::mult_of_hz(hz) };
            let millihertz = hz * 1_000;
            let error_ppb = line.millihertz().abs_diff(millihertz) as u128 * 1_000_000_000 / millihertz as u128;
            assert!(error_ppb <= 1, "{hz} Hz came back as {} mHz", line.millihertz());
            assert!(line.nanos(hz).abs_diff(1_000_000_000) <= 1, "{hz} Hz: a second is {} ns", line.nanos(hz));
            assert!(line.ticks(1_000_000_000).abs_diff(hz) <= 1 + hz / 1_000_000_000);
        }
    }

    /// Half a unit of `mult` at 2.4192 GHz is 0.28 ppb, so a day of a stated
    /// rate's ticks is a day to 25 µs; a period kept in whole femtoseconds read
    /// it 1.9 ppm short, 164 ms.
    #[test]
    fn a_day_at_a_stated_rate_is_a_day() {
        let hz = 2_419_200_000u64;
        let line = Line { counter: 0, stamp: 0, mult: Line::mult_of_hz(hz) };
        let day = 86_400u64;
        assert!(line.nanos(hz * day).abs_diff(day * 1_000_000_000) <= 25_000);
    }

    #[test]
    fn the_page_publishes_whole_lines() {
        let page = ClockPage { magic: AtomicU64::new(0), stamp_at_boot: AtomicU64::new(0), demoted: AtomicU64::new(0), line: Latch::new() };
        assert_eq!(page.read(), None);
        let first = Line { counter: 7, stamp: 9, mult: GHZ };
        page.init(9, first);
        assert_eq!(page.read(), Some(Clock { line: first, stamp_at_boot: 9 }));
        let second = Line { counter: 70, stamp: 72, mult: GHZ / 2 };
        page.publish(second);
        assert_eq!(page.read().map(|c| c.line), Some(second));
        page.publish(first);
        assert_eq!(page.read().map(|c| c.line), Some(first));
        assert!(!page.demoted());
        page.demote();
        assert!(page.demoted());
    }
}
