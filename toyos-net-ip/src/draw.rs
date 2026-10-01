//! Every random value [ip] needs is SipHash-2-4 over (secret, purpose, the purpose's counter)
//! (§1.3): one secret makes the layer deterministic, and without it no draw is predictable.

use core::time::Duration;

use toyos_net_wire::siphash::{siphash24, Key};

use crate::limits;

#[derive(Clone, Copy)]
pub(crate) enum Purpose {
    Reachable,
    Acd,
    Igmp,
}

pub(crate) fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) struct Draws {
    key: Key,
    reachable: u64,
    acd: u64,
    igmp: u64,
}

impl Draws {
    pub fn new(key: Key) -> Self {
        Self { key, reachable: 0, acd: 0, igmp: 0 }
    }

    fn next(&mut self, purpose: Purpose) -> u64 {
        let (byte, counter) = match purpose {
            Purpose::Reachable => (1, &mut self.reachable),
            Purpose::Acd => (2, &mut self.acd),
            Purpose::Igmp => (3, &mut self.igmp),
        };
        let [a, b, c, d, e, f, g, h] = counter.to_le_bytes();
        *counter = counter.wrapping_add(1);
        siphash24(&self.key, &[byte, a, b, c, d, e, f, g, h])
    }

    /// Uniform over `[low, high]`.
    pub fn between(&mut self, purpose: Purpose, low: Duration, high: Duration) -> Duration {
        let low = nanos(low);
        let span = nanos(high).saturating_sub(low).saturating_add(1);
        Duration::from_nanos(low.saturating_add(self.next(purpose).checked_rem(span).unwrap_or(0)))
    }

    /// Uniform over `(0, high]`: never at once.
    pub fn after(&mut self, purpose: Purpose, high: Duration) -> Duration {
        self.between(purpose, Duration::from_nanos(1), high.max(Duration::from_nanos(1)))
    }

    /// RFC 4861 §6.3.2: 0.5 to 1.5 times BASE_REACHABLE_TIME.
    pub fn reachable_time(&mut self) -> Duration {
        self.between(Purpose::Reachable, limits::nud::REACHABLE_MIN, limits::nud::REACHABLE_MAX)
    }

    /// The error limiter's key, derived so the two keyed functions reveal nothing of each other.
    pub fn limiter_key(&self) -> Key {
        let [a, b, c, d, e, f, g, h] = siphash24(&self.key, &[4, 0]).to_le_bytes();
        let [i, j, k, l, m, n, o, p] = siphash24(&self.key, &[4, 1]).to_le_bytes();
        [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p]
    }
}
