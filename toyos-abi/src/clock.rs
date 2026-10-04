//! The monotonic clock a process reads without a syscall.
//!
//! The kernel calibrates the counter once at boot and never again, so the two
//! numbers that turn a counter reading into nanoseconds since boot are
//! constants for the machine's life. It writes them into one page before the
//! first process exists and maps that page read-only at [`CLOCK_PAGE`] in every
//! address space; [`nanos_since_boot`] is the kernel's own arithmetic on the
//! same two words, so a stamp taken here and one taken by the kernel's log
//! order against each other.
//!
//! **No sequence word, because nothing is ever rewritten.** A clock page that a
//! kernel adjusted would need one; this one is laid out before it is mapped and
//! is immutable after, which is the whole of its concurrency argument.

/// Where every address space finds the page: below the lowest address an
/// allocation or a placed `mmap` may take, so nothing a process maps can land
/// on it, and 2 MiB-aligned because that is the page the kernel maps.
pub const CLOCK_PAGE: u64 = 0x1_0000_0000;

/// What the page opens with, so a reader that finds anything else refuses
/// rather than computes a time out of it.
pub const CLOCK_MAGIC: u64 = 0x544F_594F_5343_4C4B; // "TOYOSCLK"

/// The page's layout. Written once by the kernel before it is mapped anywhere.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockPage {
    pub magic: u64,
    /// The counter's reading at nanosecond zero.
    pub counter_at_boot: u64,
    /// One counter tick, in femtoseconds.
    pub period_fs: u64,
}

/// Nanoseconds between `counter_at_boot` and `now`, on the kernel's formula.
///
/// Saturating below boot: a counter read on a CPU that trails reads as the
/// oldest instant, never as a wrap 584 years ahead.
pub const fn nanos_between(counter_at_boot: u64, period_fs: u64, now: u64) -> u64 {
    let delta = now.saturating_sub(counter_at_boot);
    ((delta as u128 * period_fs as u128) / 1_000_000) as u64
}

/// Nanoseconds since boot, off this process's clock page.
///
/// # Panics
/// As [`page`].
pub fn nanos_since_boot() -> u64 {
    let page = page();
    let now = crate::arch::counter();
    nanos_between(page.counter_at_boot, page.period_fs, now)
}

/// This process's clock page.
///
/// # Panics
/// On a page that does not carry [`CLOCK_MAGIC`]: every address space the
/// kernel builds maps one, so its absence is a kernel that broke the ABI.
pub fn page() -> ClockPage {
    // SAFETY: the kernel maps the page read-only at `CLOCK_PAGE` in every
    // address space before the first instruction runs, and never writes it
    // again, so a volatile copy of the whole value is a read of constants.
    let page = unsafe { core::ptr::read_volatile(CLOCK_PAGE as *const ClockPage) };
    assert!(page.magic == CLOCK_MAGIC, "the clock page at {CLOCK_PAGE:#x} carries no clock");
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arithmetic the kernel's `clock::nanos_since_boot` does, so the two
    /// clocks agree on every counter value: a 1 GHz counter is one tick per
    /// nanosecond, and a counter behind boot reads zero.
    #[test]
    fn the_formula_is_the_kernels() {
        assert_eq!(nanos_between(100, 1_000_000, 1_100), 1_000);
        assert_eq!(nanos_between(100, 1_000_000, 50), 0);
        // 2.5 GHz: 400 fs a tick.
        assert_eq!(nanos_between(0, 400_000, 2_500_000_000), 1_000_000_000);
        // No overflow at a counter the machine will not reach in a century.
        assert_eq!(nanos_between(0, 1_000_000, u64::MAX), u64::MAX);
    }

    #[test]
    fn the_page_is_three_words() {
        assert_eq!(core::mem::size_of::<ClockPage>(), 24);
    }
}
