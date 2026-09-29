//! The error limiter (§9.4): a token bucket per destination in 256 keyed slots, and one global
//! bucket whose burst is redrawn every second from a keyed function. A bucket holds its tokens as
//! nanoseconds of refill, so fractions are carried and never rounded away, and a moved clock
//! refills it to its cap and no further. The TCP resets for segments nobody asked for draw from a
//! second instance: a shared budget is the side channel the per-destination buckets remove.

use alloc::vec;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_wire::siphash::{siphash24, Key};
use toyos_net_wire::Instant;

use crate::draw::nanos;
use crate::limits::icmp::{DEST_BURST, DEST_PER_S, DEST_SLOTS, GLOBAL_BURST_MAX, GLOBAL_BURST_MIN, GLOBAL_PER_S};

const SECOND: u64 = 1_000_000_000;
const DEST_COST: u64 = SECOND / DEST_PER_S;
const GLOBAL_COST: u64 = SECOND / GLOBAL_PER_S;

#[derive(Clone, Copy, Debug)]
struct Bucket {
    level: u64,
    at: Instant,
}

impl Bucket {
    fn refill(&mut self, now: Instant, cap: u64) {
        self.level = self.level.saturating_add(nanos(now.since(self.at))).min(cap);
        self.at = self.at.max(now);
    }

    fn has(&self, cost: u64) -> bool {
        self.level >= cost
    }

    fn take(&mut self, cost: u64) {
        self.level = self.level.saturating_sub(cost);
    }
}

#[derive(Clone, Debug)]
pub struct Limiter {
    key: Key,
    slots: Vec<Bucket>,
    global: Bucket,
}

impl Limiter {
    /// Every bucket starts full.
    pub fn new(key: Key) -> Self {
        let full = |burst: u64, cost: u64| Bucket { level: burst.saturating_mul(cost), at: Instant::from_nanos(0) };
        Self { key, slots: vec![full(DEST_BURST, DEST_COST); DEST_SLOTS], global: full(GLOBAL_BURST_MAX, GLOBAL_COST) }
    }

    /// The slot `destination`'s bucket lives in.
    pub fn slot(&self, destination: Ipv4Addr) -> usize {
        let [a, b, c, d] = destination.octets();
        let hash = siphash24(&self.key, &[1, a, b, c, d]);
        usize::try_from(hash.checked_rem(u64::try_from(DEST_SLOTS).unwrap_or(1)).unwrap_or(0)).unwrap_or(0)
    }

    /// The global bucket's burst during the second `now` falls in.
    pub fn global_burst(&self, now: Instant) -> u64 {
        let [a, b, c, d, e, f, g, h] = (now.nanos() / SECOND).to_le_bytes();
        let span = GLOBAL_BURST_MAX.saturating_sub(GLOBAL_BURST_MIN).saturating_add(1);
        let draw = siphash24(&self.key, &[2, a, b, c, d, e, f, g, h]).checked_rem(span).unwrap_or(0);
        GLOBAL_BURST_MIN.saturating_add(draw)
    }

    /// Takes a token from `destination`'s bucket and one from the global bucket, or refuses and
    /// takes neither.
    pub fn allow(&mut self, now: Instant, destination: Ipv4Addr) -> bool {
        let global_cap = self.global_burst(now).saturating_mul(GLOBAL_COST);
        let index = self.slot(destination);
        let Some(slot) = self.slots.get_mut(index) else { return false };
        slot.refill(now, DEST_BURST.saturating_mul(DEST_COST));
        self.global.refill(now, global_cap);
        if !(slot.has(DEST_COST) && self.global.has(GLOBAL_COST)) {
            return false;
        }
        slot.take(DEST_COST);
        self.global.take(GLOBAL_COST);
        true
    }
}
