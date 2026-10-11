//! The pass's one cadence: how long the card holds its interrupt after a pass that took frames.
//! Every frame of the next batch waits up to that long, and so does every acknowledgment the
//! batch owes, every pipe write to a client and every wake of one: they follow the pass one for
//! one, so nothing else is timed.
//!
//! **The interval is the least of three bounds, never under [`FLOOR`], and the ring's wins over
//! the floor:**
//! - **The ACK clock**: the least round trip among the streams whose data the batch moved
//!   (`toyos_net_tcp::Tcp::take_least_rtt`), over [`K`]. The interval is added to the delay of
//!   every acknowledgment and to every round-trip sample the peer takes, so it keeps at least `K`
//!   acknowledgments a round trip on the sender's clock and lengthens the round trip by at most
//!   one `K`th. A receiver's estimate already holds the interval, so under that feedback it
//!   settles at `rtt · K / (K − 1)` and does not climb. A batch that moved no stream with an
//!   estimate has no clock to keep.
//! - **The stretch**: [`STRETCH`] full-sized frames at the link's rate. A sender that does not pace
//!   answers an acknowledgment with a burst of what it acknowledges, and a queue on the path has
//!   to hold that burst or lose its tail; this bounds it whatever the round trip, at a rate slow
//!   enough that the stretch is longer than the floor. At a gigabit it is shorter, and the floor
//!   holds.
//! - **The ring**: the descriptors the device holds empty above the count it reports itself short
//!   at must take one interval and one interval of lateness of full-sized frames at the link's
//!   rate.
//! - **[`FLOOR`]**: what any peer's frames can make of netstack's wakes, a flood for nobody's or a
//!   stream's on a short path. The ring wins over it: a wake too many costs cycles, and a ring
//!   that overruns loses frames.

use std::num::NonZeroU32;
use std::time::Duration;

/// Acknowledgments a round trip, at the least, and the inverse of the most the interval
/// lengthens a round trip by.
pub const K: u32 = 32;

/// The most full-sized frames a batch holds at the link's rate above [`FLOOR`], and so the most
/// an acknowledgment releases from a sender that does not pace.
pub const STRETCH: u64 = 8;

/// The least interval. A wake — the kernel's delivery of the interrupt and netstack's pass around
/// its batch — costs about 30 µs on the hardware ToyOS targets, so wakes this far apart take under
/// a fifth of a CPU; and the I219, which rounds an interval down by at most a fifth, still holds
/// them 128 µs apart.
pub const FLOOR: Duration = Duration::from_micros(160);

/// A full-sized frame as the link carries it, in bits: the largest frame the stack sends or
/// takes (`toyos_net_ip::FRAME`), its FCS, the preamble and start delimiter, and the interframe
/// gap (IEEE 802.3 clause 4: 4, 8 and 12 bytes).
const WIRE_FRAME_BITS: u64 = (toyos_net_ip::FRAME as u64 + 4 + 8 + 12) * 8;

/// The receive ring, as the pass leaves it.
#[derive(Clone, Copy, Debug)]
pub struct Ring {
    /// Descriptors the device holds empty, every buffer the pass took given back, above the count
    /// at which it reports itself short.
    pub room: usize,
    /// The link's rate.
    pub mbps: NonZeroU32,
}

/// How long the card holds its interrupt after a pass whose batch moved streams with
/// `least_rtt` as their least round trip, on `ring`.
pub fn cadence(least_rtt: Option<Duration>, ring: Ring) -> Duration {
    let frames = |n: u64| Duration::from_nanos(n * WIRE_FRAME_BITS * 1000 / u64::from(ring.mbps.get()));
    let stretch = frames(STRETCH);
    let clock = least_rtt.map_or(stretch, |rtt| (rtt / K).min(stretch));
    clock.max(FLOOR).min(frames(ring.room as u64) / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIGABIT: NonZeroU32 = NonZeroU32::new(1000).unwrap();
    const HUNDRED_MBPS: NonZeroU32 = NonZeroU32::new(100).unwrap();

    /// The rates an I219 links at.
    const I219_RATES: [u32; 3] = [10, 100, 1000];

    /// A ring of 1,000 that reports itself short at half, as a pass that took every frame leaves it.
    fn ring(mbps: NonZeroU32) -> Ring {
        Ring { room: 499, mbps }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// 1,538 bytes on the wire for a 1,500-byte packet, 12,304 ns of a gigabit link.
    const GIGABIT_FRAME: Duration = Duration::from_nanos(12_304);
    const HUNDRED_MBPS_FRAME: Duration = Duration::from_nanos(123_040);

    #[test]
    fn a_full_sized_frame_takes_12_304_ns_at_a_gigabit() {
        assert_eq!(WIRE_FRAME_BITS, 1538 * 8);
        assert_eq!(cadence(Some(Duration::MAX), Ring { room: 2, mbps: GIGABIT }), GIGABIT_FRAME);
    }

    /// At every link rate from 10 Mb/s to 400 Gb/s, every round trip and every ring, the full-sized
    /// frames that arrive at line rate in two intervals — one and its lateness — fit in what the
    /// ring holds above its reserve, the floor included.
    #[test]
    fn the_interval_never_exceeds_what_the_ring_holds_at_any_link_rate() {
        for mbps in [10, 100, 1000, 2500, 10_000, 25_000, 100_000, 400_000] {
            let mbps = NonZeroU32::new(mbps).unwrap();
            for room in [0, 1, 64, 255, 499, 4095, 65_535] {
                let ring = Ring { room, mbps };
                for rtt in [None, Some(Duration::ZERO), Some(Duration::from_micros(200)), Some(ms(15)), Some(ms(300)), Some(Duration::MAX)] {
                    let interval = cadence(rtt, ring);
                    let bits = 2 * interval.as_nanos() * u128::from(mbps.get()) / 1000;
                    let arrive = bits / u128::from(WIRE_FRAME_BITS);
                    assert!(arrive <= room as u128, "{mbps} Mb/s, room {room}, rtt {rtt:?}: {interval:?} brings {arrive} frames into {room}");
                }
            }
        }
    }

    /// Whatever a batch moved — nothing with a round trip, as a flood for nobody or a datagram flow
    /// does, or a stream on any path — a pass that took every frame holds an I219 at any of its
    /// rates at least 128 µs, as the part applies it: no peer wakes netstack more often.
    #[test]
    fn no_batch_holds_an_i219_under_128_us() {
        for mbps in I219_RATES {
            for rtt in [None, Some(Duration::ZERO), Some(Duration::from_micros(200)), Some(ms(1)), Some(ms(15)), Some(Duration::MAX)] {
                let asked = cadence(rtt, ring(NonZeroU32::new(mbps).unwrap()));
                let nanos = u64::try_from(asked.as_nanos()).unwrap();
                let applied = u64::from(toyos_i219::throttle_units(nanos)) * toyos_i219::THROTTLE_UNIT_NANOS;
                assert!(applied >= 128_000, "{mbps} Mb/s, rtt {rtt:?}: asked {asked:?}, the part holds {applied} ns");
            }
        }
    }

    /// A batch that moved no stream with a round trip is held to the stretch, and to the floor
    /// where the stretch is shorter: at a gigabit, `STRETCH` frames are.
    #[test]
    fn a_batch_with_no_round_trip_is_held_to_the_stretch_above_the_floor() {
        assert_eq!(cadence(None, ring(HUNDRED_MBPS)), HUNDRED_MBPS_FRAME * STRETCH as u32);
        assert_eq!(cadence(None, ring(GIGABIT)), FLOOR);
    }

    /// The bound is a `K`th of the least round trip between the floor and the stretch: a round
    /// trip short enough to put it under the floor gets the floor, and one long enough to put it
    /// over the stretch gets the stretch.
    #[test]
    fn the_least_round_trip_over_k_is_the_interval_between_the_floor_and_the_stretch() {
        let ten_mbps = ring(NonZeroU32::new(10).unwrap());
        assert_eq!(cadence(Some(ms(16)), ten_mbps), Duration::from_micros(500));
        assert_eq!(cadence(Some(ms(15)), ten_mbps), Duration::from_nanos(468_750));
        assert_eq!(cadence(Some(FLOOR * K - Duration::from_nanos(K.into())), ten_mbps), FLOOR);
        assert_eq!(cadence(Some(ms(40)), ring(HUNDRED_MBPS)), HUNDRED_MBPS_FRAME * STRETCH as u32);
    }

    /// A short path's stream beside a long path's sets the interval: the batch reports its least
    /// round trip, and that is what bounds it. A long path alone is bounded by the stretch, and on
    /// a ring with less room than the stretch, by the ring.
    #[test]
    fn a_short_round_trip_bounds_the_interval_and_a_long_one_meets_the_stretch_or_the_ring() {
        let ten_mbps = ring(NonZeroU32::new(10).unwrap());
        assert_eq!(cadence(Some(ms(8)), ten_mbps), Duration::from_micros(250));
        assert_eq!(cadence(Some(ms(1000)), ten_mbps), Duration::from_nanos(1_230_400) * STRETCH as u32);
        let short = Ring { room: 20, mbps: GIGABIT };
        assert_eq!(cadence(Some(ms(1000)), short), GIGABIT_FRAME * 10);
    }

    /// The interval lengthens the round trip the receiver measures by up to itself, and the
    /// next interval is taken from that estimate. From the path's own round trip and from an
    /// estimate far above it alike, the estimate moves only toward `rtt · K / (K − 1)` and
    /// settles there: it never ratchets past it. Each path's `K`th clears the floor and is
    /// under the stretch of a 10 Mb/s link.
    #[test]
    fn under_its_own_feedback_the_round_trip_settles_and_does_not_ratchet() {
        let ten_mbps = ring(NonZeroU32::new(10).unwrap());
        for path in [ms(6), ms(15), ms(40)] {
            let settled = path * K / (K - 1);
            for start in [path, ms(10_000)] {
                let mut estimate = start;
                for _ in 0..200 {
                    let next = path + cadence(Some(estimate), ten_mbps);
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
