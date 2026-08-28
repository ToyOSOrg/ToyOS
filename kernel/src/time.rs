//! A duration is typed by what its expiry means: [`Bound`], [`Cadence`],
//! [`Tripwire`], [`Deadline`], [`Floor`], [`Budget`], [`Delay`]. No kind is
//! constructible from a bare magnitude; every constructor takes a
//! `&'static str` justification beside the number.
//!
//! This file names nothing outside `core`: `kernel-loom` compiles it a
//! second time to carry an [`Instant`] into its model, and a dependency on
//! a subject would break that.

use core::fmt;
use core::ops::{Add, Sub};

/// A point on the machine's monotonic clock; only [`clock::now`](crate::clock::now)
/// mints one. Its `Add`/`Sub` arithmetic saturates instead of panicking, and
/// there is no `Add<Instant>`, so relative and absolute can't be confused.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Instant(u64);

impl Instant {
    /// Mints an `Instant` from a raw nanosecond reading — `clock.rs` and the
    /// syscall boundary are the two legitimate callers.
    pub const fn from_nanos_since_boot(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Leaves the type system that keeps an instant from being confused with a duration.
    pub const fn nanos_since_boot(self) -> u64 {
        self.0
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, rhs: Duration) -> Instant {
        Instant(self.0.saturating_add(rhs.0))
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, rhs: Instant) -> Duration {
        Duration(self.0.saturating_sub(rhs.0))
    }
}

/// An amount of time: the magnitude a kind carries, never a kind itself.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Duration(u64);

impl Duration {
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    pub const fn from_micros(micros: u64) -> Self {
        Self(micros * 1_000)
    }

    pub const fn from_millis(millis: u64) -> Self {
        Self(millis * 1_000_000)
    }

    pub const fn from_secs(secs: u64) -> Self {
        Self(secs * 1_000_000_000)
    }

    pub const fn nanos(self) -> u64 {
        self.0
    }

    /// Truncates to milliseconds, like the `/ 1_000_000` it replaces.
    pub const fn millis(self) -> u64 {
        self.0 / 1_000_000
    }
}

impl fmt::Display for Duration {
    /// Not used on a hot path.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 >= 1_000_000 {
            write!(f, "{}ms", self.millis())
        } else {
            write!(f, "{}ns", self.nanos())
        }
    }
}

/// An absolute instant a caller chose to stop waiting at. Built only through
/// its three named constructors, never from a bare `u64`, so a
/// `0`-means-no-timeout convention can't silently collide with a real deadline.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Deadline(u64);

impl Deadline {
    /// Stop at this instant.
    pub const fn at(instant: Instant) -> Self {
        Self(instant.0)
    }

    /// Do not stop; `u64::MAX` ns is unreachable because the clock saturates rather than wraps.
    pub const fn never() -> Self {
        Self(u64::MAX)
    }

    /// Already over: evaluate once, then answer.
    pub const fn passed() -> Self {
        Self(0)
    }

    /// Whether `now` is at or past it.
    pub const fn reached(self, now: Instant) -> bool {
        now.0 >= self.0
    }

    pub const fn is_never(self) -> bool {
        self.0 == u64::MAX
    }

    /// The absolute nanosecond the scheduler's `Nanos` wants; check [`Deadline::is_never`] first.
    pub const fn nanos(self) -> u64 {
        self.0
    }
}

/// The contract that says the thing will happen: sourced from a device
/// register or a cited spec section, and its expiry means the device broke —
/// a named error, never a retry.
#[derive(Clone, Copy)]
pub struct Bound {
    limit: Duration,
    cite: &'static str,
}

impl Bound {
    /// A bound a specification states. `cite` is the section.
    pub const fn from_spec(limit: Duration, cite: &'static str) -> Self {
        Self { limit, cite }
    }

    /// A bound a device register publishes; `cite` names the register. Not
    /// `const`: the number is read off the hardware at the call site.
    pub fn from_register(limit: Duration, cite: &'static str) -> Self {
        Self { limit, cite }
    }

    pub const fn duration(self) -> Duration {
        self.limit
    }

    pub const fn nanos(self) -> u64 {
        self.limit.nanos()
    }
}

impl fmt::Display for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.limit, self.cite)
    }
}

/// How often a thing may be re-done, and what makes that rate affordable.
/// Nothing expires; a `Cadence` is a rate, not a register-poll limit.
#[derive(Clone, Copy)]
pub struct Cadence {
    period: Duration,
    why: &'static str,
}

impl Cadence {
    pub const fn every(period: Duration, why: &'static str) -> Self {
        Self { period, why }
    }

    pub const fn duration(self) -> Duration {
        self.period
    }

    pub const fn nanos(self) -> u64 {
        self.period.nanos()
    }
}

impl fmt::Display for Cadence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.period, self.why)
    }
}

/// A duration whose expiry is a **panic**: the machine is broken, fail fast.
/// The argument is how long is absurd, not where the number came from — a
/// tripwire that could be cited would be a [`Bound`]. A tripwire smaller than
/// the largest [`Bound`] on the path it covers panics a healthy machine.
#[derive(Clone, Copy)]
pub struct Tripwire {
    limit: Duration,
    absurd_because: &'static str,
}

impl Tripwire {
    pub const fn absurd(limit: Duration, absurd_because: &'static str) -> Self {
        Self { limit, absurd_because }
    }

    pub const fn duration(self) -> Duration {
        self.limit
    }

    pub const fn nanos(self) -> u64 {
        self.limit.nanos()
    }
}

impl fmt::Display for Tripwire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.limit, self.absurd_because)
    }
}

/// A wall-clock allowance whose expiry is a **degraded answer**, never a panic.
#[derive(Clone, Copy)]
pub struct Budget {
    limit: Duration,
    degraded: &'static str,
}

impl Budget {
    /// `degraded` is what the caller answers with when it is spent.
    pub const fn of(limit: Duration, degraded: &'static str) -> Self {
        Self { limit, degraded }
    }

    pub const fn duration(self) -> Duration {
        self.limit
    }

    pub const fn nanos(self) -> u64 {
        self.limit.nanos()
    }
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.limit, self.degraded)
    }
}

/// A duration used as a bound on *another* duration, never as a wait.
/// Nothing expires; there is no caller and no register.
#[derive(Clone, Copy)]
pub struct Floor {
    least: Duration,
    why: &'static str,
}

impl Floor {
    /// `why` states what bounds it below and above.
    pub const fn policy(least: Duration, why: &'static str) -> Self {
        Self { least, why }
    }

    pub const fn nanos(self) -> u64 {
        self.least.nanos()
    }
}

impl fmt::Display for Floor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.least, self.why)
    }
}

/// A duration the caller **spends**: nothing expires, nothing is waited for,
/// and the elapsing is the success path.
#[derive(Clone, Copy)]
pub struct Delay {
    span: Duration,
    why: &'static str,
}

impl Delay {
    /// A settle a specification mandates. `cite` is the section.
    pub const fn from_spec(span: Duration, cite: &'static str) -> Self {
        Self { span, why: cite }
    }

    /// A window spent measuring something across it; `why` says what the
    /// length buys — accuracy against boot time, always.
    pub const fn to_measure(span: Duration, why: &'static str) -> Self {
        Self { span, why }
    }

    pub const fn nanos(self) -> u64 {
        self.span.nanos()
    }
}

impl fmt::Display for Delay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.span, self.why)
    }
}
