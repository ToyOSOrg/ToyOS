//! The pass's one cadence: how long the card holds its interrupt after a pass that took frames.
//! Every frame of the next batch waits up to that long, and so does every acknowledgment the
//! batch owes, every pipe write to a client and every wake of one: they follow the pass one for
//! one, so nothing else is timed.
//!
//! **The interval is the lesser of two bounds, and never under [`FLOOR`] unless the ring says so:**
//! - **The ACK clock**: the least round trip among the streams whose data the batch moved
//!   (`toyos_net_tcp::Tcp::take_least_rtt`), over [`K`]. The interval is added to the delay of
//!   every acknowledgment and to every round-trip sample the peer takes, so it keeps at least `K`
//!   acknowledgments a round trip on the sender's clock and lengthens the round trip by at most
//!   one `K`th. A receiver's estimate already holds the interval, so under that feedback it
//!   settles at `rtt · K / (K − 1)` and does not climb. A batch that moved no stream with an
//!   estimate is held to [`FLOOR`].
//! - **The ring**: the descriptors the device holds empty after the pass, above the
//!   [`Ring::reserve`] it reports itself short at, must take one interval and one interval of
//!   lateness of full-sized frames at the link's rate.
//! - **[`FLOOR`]**: interrupts come no faster than the wake path costs, so a flood cannot make
//!   the wakes themselves the load. The ring wins over it: a wake too many costs cycles, and a
//!   ring that overruns loses frames.

use std::num::NonZeroU32;
use std::time::Duration;

/// Acknowledgments a round trip, at the least, and the inverse of the most the interval
/// lengthens a round trip by.
pub static K: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(16);

/// The least interval: an estimate of one wake's cost — the kernel's delivery of the interrupt
/// and netstack's pass around its batch — on the hardware ToyOS targets.
pub const FLOOR: Duration = Duration::from_micros(30);

/// A full-sized frame as the link carries it, in bits: the largest frame the stack sends or
/// takes (`toyos_net_ip::FRAME`), its FCS, the preamble and start delimiter, and the interframe
/// gap (IEEE 802.3 clause 4: 4, 8 and 12 bytes).
const WIRE_FRAME_BITS: u64 = (toyos_net_ip::FRAME as u64 + 4 + 8 + 12) * 8;

/// The receive ring, as the pass leaves it.
#[derive(Clone, Copy, Debug)]
pub struct Ring {
    /// Descriptors the device holds empty, every buffer the pass took given back.
    pub free: usize,
    /// Free descriptors at which the device reports itself short.
    pub reserve: usize,
    /// The link's rate.
    pub mbps: NonZeroU32,
}

/// How long the card holds its interrupt after a pass whose batch moved streams with
/// `least_rtt` as their least round trip, on `ring`.
pub fn cadence(least_rtt: Option<Duration>, ring: Ring) -> Duration {
    let clock = least_rtt.map_or(FLOOR, |rtt| (rtt / K.load(std::sync::atomic::Ordering::Relaxed)).max(FLOOR));
    let frames = ring.free.saturating_sub(ring.reserve) as u64;
    let ring_nanos = frames * WIRE_FRAME_BITS * 1000 / u64::from(ring.mbps.get()) / 2;
    clock.min(Duration::from_nanos(ring_nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIGABIT: NonZeroU32 = NonZeroU32::new(1000).unwrap();

    /// A ring of 1,000 that reports itself short at half, as the pass leaves it.
    fn ring(mbps: NonZeroU32) -> Ring {
        Ring { free: 999, reserve: 500, mbps }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// What `ring` holds above its reserve, at a gigabit: 499 full-sized frames, over two.
    const GIGABIT_RING: Duration = Duration::from_nanos(499 * 12_304 / 2);

    /// 1,538 bytes on the wire for a 1,500-byte packet, 12,304 ns of a gigabit link.
    #[test]
    fn a_full_sized_frame_takes_12_304_ns_at_a_gigabit() {
        assert_eq!(WIRE_FRAME_BITS, 1538 * 8);
        assert_eq!(cadence(Some(Duration::MAX), Ring { free: 2, reserve: 0, mbps: GIGABIT }), Duration::from_nanos(12_304));
    }

    /// At every link rate from 10 Mb/s to 400 Gb/s, every round trip and every ring, the full-sized
    /// frames that arrive at line rate in two intervals — one and its lateness — fit in what the
    /// ring holds above its reserve, the floor included.
    #[test]
    fn the_interval_never_exceeds_what_the_ring_holds_at_any_link_rate() {
        for mbps in [10, 100, 1000, 2500, 10_000, 25_000, 100_000, 400_000] {
            let mbps = NonZeroU32::new(mbps).unwrap();
            for free in [0, 1, 64, 255, 500, 501, 999, 4095, 65_535] {
                let ring = Ring { free, reserve: free.min(500), mbps };
                for rtt in [None, Some(Duration::ZERO), Some(Duration::from_micros(200)), Some(ms(15)), Some(ms(300)), Some(Duration::MAX)] {
                    let interval = cadence(rtt, ring);
                    let bits = 2 * interval.as_nanos() * u128::from(mbps.get()) / 1000;
                    let arrive = bits / u128::from(WIRE_FRAME_BITS);
                    let room = (free - ring.reserve) as u128;
                    assert!(arrive <= room, "{mbps} Mb/s, {free} free, rtt {rtt:?}: {interval:?} brings {arrive} frames into {room}");
                }
            }
        }
    }

    #[test]
    fn a_batch_with_no_round_trip_is_held_to_the_floor() {
        assert_eq!(cadence(None, ring(GIGABIT)), FLOOR);
    }

    /// The bound is a `K`th of the least round trip, and a round trip short enough to put that
    /// under the floor gets the floor.
    #[test]
    fn the_least_round_trip_over_k_is_the_interval_above_the_floor() {
        assert_eq!(cadence(Some(ms(16)), ring(GIGABIT)), ms(1));
        assert_eq!(cadence(Some(ms(15)), ring(GIGABIT)), Duration::from_micros(937) + Duration::from_nanos(500));
        assert_eq!(cadence(Some(FLOOR * K - Duration::from_nanos(K.into())), ring(GIGABIT)), FLOOR);
    }

    /// A short path's stream beside a long path's sets the interval: the batch reports its least
    /// round trip, and that is what bounds it. A long path alone is bounded by the ring.
    #[test]
    fn a_short_round_trip_bounds_the_interval_and_a_long_one_meets_the_ring() {
        assert_eq!(cadence(Some(ms(2)), ring(GIGABIT)), Duration::from_micros(125));
        assert_eq!(cadence(Some(ms(300)), ring(GIGABIT)), GIGABIT_RING);
        assert!(GIGABIT_RING < ms(300) / K);
    }

    /// The interval lengthens the round trip the receiver measures by up to itself, and the
    /// next interval is taken from that estimate. From the path's own round trip and from an
    /// estimate far above it alike, the estimate moves only toward `rtt · K / (K − 1)` and
    /// settles there: it never ratchets past it.
    #[test]
    fn under_its_own_feedback_the_round_trip_settles_and_does_not_ratchet() {
        for path in [Duration::from_micros(500), ms(15), ms(40)] {
            let settled = path * K / (K - 1);
            for start in [path, ms(10_000)] {
                let mut estimate = start;
                for _ in 0..200 {
                    let next = path + cadence(Some(estimate), ring(GIGABIT));
                    let toward = if start < settled { estimate <= next && next <= settled } else { settled <= next && next <= estimate };
                    assert!(toward, "{path:?} from {start:?}: {estimate:?} moved to {next:?}, away from {settled:?}");
                    estimate = next;
                }
                let off = settled.abs_diff(estimate);
                assert!(off <= Duration::from_micros(1), "{path:?} from {start:?}: settled at {estimate:?}, not {settled:?}");
            }
        }
    }
}
