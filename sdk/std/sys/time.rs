use crate::time::Duration;

/// Nanoseconds since boot, as the clock page counts them: a reading is the
/// page's and no arithmetic after it.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Instant(u64);

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SystemTime(Duration);

pub const UNIX_EPOCH: SystemTime = SystemTime(Duration::from_secs(0));

impl Instant {
    #[inline]
    pub fn now() -> Instant {
        Instant(toyos_abi::clock::nanos_since_boot())
    }

    pub fn checked_sub_instant(&self, other: &Instant) -> Option<Duration> {
        self.0.checked_sub(other.0).map(Duration::from_nanos)
    }

    pub fn checked_add_duration(&self, other: &Duration) -> Option<Instant> {
        Some(Instant(self.0.checked_add(u64::try_from(other.as_nanos()).ok()?)?))
    }

    pub fn checked_sub_duration(&self, other: &Duration) -> Option<Instant> {
        Some(Instant(self.0.checked_sub(u64::try_from(other.as_nanos()).ok()?)?))
    }
}

impl SystemTime {
    pub const MAX: SystemTime = SystemTime(Duration::MAX);

    pub const MIN: SystemTime = SystemTime(Duration::ZERO);

    /// Panics on a machine whose clock will not answer, as the unix
    /// implementation does for a failing `clock_gettime`: 1970 would be
    /// indistinguishable from a machine that really is at the epoch.
    pub fn now() -> SystemTime {
        let secs = toyos_abi::syscall::clock_epoch()
            .expect("SYS_CLOCK_EPOCH: this machine will not say what time it is");
        SystemTime(Duration::from_secs(secs))
    }

    pub fn sub_time(&self, other: &SystemTime) -> Result<Duration, Duration> {
        self.0.checked_sub(other.0).ok_or_else(|| other.0 - self.0)
    }

    pub fn checked_add_duration(&self, other: &Duration) -> Option<SystemTime> {
        Some(SystemTime(self.0.checked_add(*other)?))
    }

    pub fn checked_sub_duration(&self, other: &Duration) -> Option<SystemTime> {
        Some(SystemTime(self.0.checked_sub(*other)?))
    }

    pub(crate) fn from_nanos(nanos: u64) -> SystemTime {
        SystemTime(Duration::from_nanos(nanos))
    }

    /// Nanoseconds since the Unix epoch, as a file's time is kept: `None`
    /// past what a `u64` counts, the year 2554.
    pub(crate) fn nanos(&self) -> Option<u64> {
        u64::try_from(self.0.as_nanos()).ok()
    }
}
