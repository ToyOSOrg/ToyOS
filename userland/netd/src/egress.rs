//! The frames the stack has made and the transmit ring has no slot for yet.
//!
//! **RFC 8290's FQ-CoDel**: a frame joins its flow's queue, the ring takes one
//! frame from each flow with frames waiting in turn, and each flow's queue
//! runs RFC 8289's CoDel at its head. So a flow that makes frames faster than
//! the ring drains them — a ping flood's replies, one upload on a slow link —
//! queues behind itself, and a queue that has held frames past [`TARGET`] for
//! an [`INTERVAL`] starts dropping at its head, which tells its sender to slow
//! down while the delay is still well under a retransmission timeout. Without
//! that, the stack learns of a full ring only by its timers: segments sat here
//! past their RTO, went again, and the ring carried retransmissions of what it
//! had not yet sent once.
//!
//! A push that takes the total past [`LIMIT`] drops the head of whichever flow
//! holds the most (§4.1.1): the backstop for a load CoDel's rate cannot catch.
//!
//! Flows are hash buckets (§4.1.1), keyed by a secret so a peer cannot aim two
//! flows at one bucket; two that share one are one flow, as in the RFC. What a
//! frame's flow is [`Flow::of`] reads off its own headers; a frame the reading
//! does not recognise is its EtherType's flow. Round robin is one frame a
//! turn, where the RFC's deficit counts bytes: every frame here is one the
//! ring takes whole, and a lookup's query waits at most a frame per other
//! flow either way.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::time::Duration;

/// How many frames may wait across every flow before the fattest flow's
/// oldest is dropped: a thousand full frames, about 1.5 MiB. Linux's
/// `fq_codel` defaults to ten times that, for a link that drains ten times
/// faster than this machine's slowest ring.
pub const LIMIT: usize = 1024;

/// The flow buckets, RFC 8290 §5.1's default.
const BUCKETS: usize = 1024;

/// The sojourn a queue may hold at its head for good (RFC 8289 §4.4).
pub const TARGET: Duration = Duration::from_millis(5);

/// How long the sojourn has to stay past [`TARGET`] before the first drop,
/// and the drop rate's scale (RFC 8289 §4.3).
pub const INTERVAL: Duration = Duration::from_millis(100);

/// A queue holding no more than one full frame is never dropped from: the
/// ring cannot empty it faster (RFC 8289 §5.2's `MAXPACKET`).
const ONE_FRAME: usize = 1514;

/// The frames one conversation makes, as its headers name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flow {
    /// TCP or UDP, by protocol and both endpoints.
    Ports { proto: u8, src: [u8; 4], dst: [u8; 4], sport: u16, dport: u16 },
    /// Any other IPv4 protocol, by protocol and destination.
    Ip { proto: u8, dst: [u8; 4] },
    /// Anything that is not IPv4, by EtherType.
    Link(u16),
}

impl Flow {
    /// The flow an Ethernet frame belongs to.
    pub fn of(frame: &[u8]) -> Self {
        let Some(ether_type) = frame.get(12..14).map(|t| u16::from_be_bytes([t[0], t[1]])) else {
            return Self::Link(0);
        };
        let ip = &frame[14..];
        if ether_type != 0x0800 || ip.len() < 20 || ip[0] >> 4 != 4 {
            return Self::Link(ether_type);
        }
        let header = usize::from(ip[0] & 0x0f) * 4;
        let proto = ip[9];
        let src = [ip[12], ip[13], ip[14], ip[15]];
        let dst = [ip[16], ip[17], ip[18], ip[19]];
        match (proto, ip.get(header..header + 4)) {
            (6 | 17, Some(p)) => Self::Ports {
                proto,
                src,
                dst,
                sport: u16::from_be_bytes([p[0], p[1]]),
                dport: u16::from_be_bytes([p[2], p[3]]),
            },
            _ => Self::Ip { proto, dst },
        }
    }
}

/// One flow's CoDel state (RFC 8289 §5).
#[derive(Default)]
struct Codel {
    /// When the sojourn, past [`TARGET`] since, will have been so for an
    /// [`INTERVAL`].
    first_above: Option<Duration>,
    dropping: bool,
    drop_next: Duration,
    count: u32,
    lastcount: u32,
}

/// `t` plus [`INTERVAL`] over the square root of `count` (RFC 8289 §5.4).
fn control_law(t: Duration, count: u32) -> Duration {
    t + INTERVAL.div_f64(f64::from(count.max(1)).sqrt())
}

/// One flow's frames, each with the time it joined, and its CoDel state.
#[derive(Default)]
struct Bucket {
    frames: VecDeque<(Duration, Vec<u8>)>,
    bytes: usize,
    codel: Codel,
}

/// The waiting frames, and what `inspect` reads about them.
pub struct Egress {
    key: u64,
    buckets: Vec<Bucket>,
    /// The buckets holding frames, in the order the ring serves them.
    turn: VecDeque<usize>,
    /// Frames waiting across every flow.
    waiting: usize,
    /// The most that have waited at once.
    pub most: usize,
    /// Frames dropped at [`LIMIT`].
    pub dropped: u64,
    /// Frames CoDel dropped at a queue's head.
    pub delay_dropped: u64,
}

impl Egress {
    /// An empty queue whose flows are hashed under `key`.
    pub fn new(key: u64) -> Self {
        Self {
            key,
            buckets: (0..BUCKETS).map(|_| Bucket::default()).collect(),
            turn: VecDeque::new(),
            waiting: 0,
            most: 0,
            dropped: 0,
            delay_dropped: 0,
        }
    }

    fn bucket_of(&self, frame: &[u8]) -> usize {
        let mut hasher = std::hash::DefaultHasher::new();
        self.key.hash(&mut hasher);
        Flow::of(frame).hash(&mut hasher);
        (hasher.finish() % BUCKETS as u64) as usize
    }

    /// Queue `frame`, made at `now`, behind its flow's, and drop the fattest
    /// flow's oldest if that takes the total past [`LIMIT`].
    pub fn push(&mut self, frame: Vec<u8>, now: Duration) {
        let b = self.bucket_of(&frame);
        let bucket = &mut self.buckets[b];
        if bucket.frames.is_empty() {
            self.turn.push_back(b);
        }
        bucket.bytes += frame.len();
        bucket.frames.push_back((now, frame));
        self.waiting += 1;
        if self.waiting > LIMIT {
            let fattest = self
                .turn
                .iter()
                .copied()
                .max_by_key(|&b| self.buckets[b].frames.len())
                .expect("frames are waiting");
            self.take(fattest).expect("the fattest flow holds a frame");
            if self.buckets[fattest].frames.is_empty() {
                self.turn.retain(|&b| b != fattest);
            }
            self.dropped += 1;
        }
        self.most = self.most.max(self.waiting);
    }

    /// The next frame in turn at `now`: CoDel's pick from the flow at the head
    /// of the turn, which goes to the back of it while it holds more.
    pub fn pop(&mut self, now: Duration) -> Option<Vec<u8>> {
        loop {
            let b = self.turn.pop_front()?;
            let frame = self.dequeue(b, now);
            if !self.buckets[b].frames.is_empty() {
                self.turn.push_back(b);
            }
            if frame.is_some() {
                return frame;
            }
        }
    }

    /// Whether any frame waits.
    pub fn is_empty(&self) -> bool {
        self.waiting == 0
    }

    /// How many frames wait now.
    pub fn len(&self) -> usize {
        self.waiting
    }

    /// Take bucket `b`'s oldest frame and when it joined.
    fn take(&mut self, b: usize) -> Option<(Duration, Vec<u8>)> {
        let bucket = &mut self.buckets[b];
        let (at, frame) = bucket.frames.pop_front()?;
        bucket.bytes -= frame.len();
        self.waiting -= 1;
        Some((at, frame))
    }

    /// RFC 8289 §5.3's `dodequeue`: the head, and whether its sojourn has
    /// stayed past [`TARGET`] for an [`INTERVAL`].
    fn head(&mut self, b: usize, now: Duration) -> Option<(Vec<u8>, bool)> {
        let Some((at, frame)) = self.take(b) else {
            self.buckets[b].codel.first_above = None;
            return None;
        };
        let bucket = &mut self.buckets[b];
        let sojourn = now.saturating_sub(at);
        let mut ok_to_drop = false;
        if sojourn < TARGET || bucket.bytes < ONE_FRAME {
            bucket.codel.first_above = None;
        } else {
            match bucket.codel.first_above {
                None => bucket.codel.first_above = Some(now + INTERVAL),
                Some(at) if now >= at => ok_to_drop = true,
                Some(_) => {}
            }
        }
        Some((frame, ok_to_drop))
    }

    /// RFC 8289 §5.5's `dequeue`, over bucket `b`.
    fn dequeue(&mut self, b: usize, now: Duration) -> Option<Vec<u8>> {
        let Some((mut frame, mut ok_to_drop)) = self.head(b, now) else {
            self.buckets[b].codel.dropping = false;
            return None;
        };
        if self.buckets[b].codel.dropping {
            if !ok_to_drop {
                self.buckets[b].codel.dropping = false;
            }
            while self.buckets[b].codel.dropping && now >= self.buckets[b].codel.drop_next {
                self.delay_dropped += 1;
                self.buckets[b].codel.count += 1;
                let Some((next, ok)) = self.head(b, now) else {
                    self.buckets[b].codel.dropping = false;
                    return None;
                };
                (frame, ok_to_drop) = (next, ok);
                let codel = &mut self.buckets[b].codel;
                if !ok_to_drop {
                    codel.dropping = false;
                } else {
                    codel.drop_next = control_law(codel.drop_next, codel.count);
                }
            }
        } else if ok_to_drop {
            self.delay_dropped += 1;
            let next = self.head(b, now);
            let codel = &mut self.buckets[b].codel;
            codel.dropping = true;
            let delta = codel.count.saturating_sub(codel.lastcount);
            codel.count = if delta > 1 && now.saturating_sub(codel.drop_next) < INTERVAL * 16 { delta } else { 1 };
            codel.drop_next = control_law(now, codel.count);
            codel.lastcount = codel.count;
            let Some((next, _)) = next else {
                codel.dropping = false;
                return None;
            };
            frame = next;
        }
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn udp(sport: u16, tag: u8) -> Vec<u8> {
        let mut f = vec![0u8; 14 + 20 + 8 + 1];
        f[12] = 0x08;
        f[14] = 0x45;
        f[14 + 9] = 17;
        f[14 + 16..14 + 20].copy_from_slice(&[10, 0, 2, 2]);
        f[34..36].copy_from_slice(&sport.to_be_bytes());
        f[36..38].copy_from_slice(&53u16.to_be_bytes());
        f[42] = tag;
        f
    }

    fn icmp(tag: u8) -> Vec<u8> {
        let mut f = udp(0, tag);
        f[14 + 9] = 1;
        f
    }

    /// A full frame of one TCP flow, `tag` in its first payload byte.
    fn segment(tag: u8) -> Vec<u8> {
        let mut f = udp(80, tag);
        f[14 + 9] = 6;
        f.resize(ONE_FRAME, 0);
        f
    }

    /// The key every test hashes under: one where the flows they use share
    /// no bucket, which the first test checks.
    const KEY: u64 = 1;

    fn egress() -> Egress {
        Egress::new(KEY)
    }

    const T0: Duration = Duration::ZERO;

    #[test]
    fn the_tests_flows_share_no_bucket() {
        let e = egress();
        let buckets = [e.bucket_of(&udp(40000, 0)), e.bucket_of(&udp(1, 0)), e.bucket_of(&icmp(0)), e.bucket_of(&segment(0))];
        let mut unique = buckets.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), buckets.len(), "{buckets:?}");
    }

    /// **A flow queued behind a flood leaves before the flood's backlog**:
    /// the ring serves one frame a flow in turn.
    #[test]
    fn a_flow_behind_a_flood_waits_one_turn() {
        let mut e = egress();
        for i in 0..100 {
            e.push(icmp(i), T0);
        }
        e.push(udp(40000, 7), T0);
        assert_eq!(e.pop(T0).unwrap()[42], 0, "the flood's first");
        assert_eq!(e.pop(T0).unwrap()[42], 7, "the query, next in turn");
        assert_eq!(e.pop(T0).unwrap()[42], 1, "then the flood again");
        assert_eq!(e.len(), 98);
    }

    /// **Past the limit the fattest flow's oldest frame goes**, and a small
    /// flow keeps every frame.
    #[test]
    fn an_overload_drops_the_fattest_flows_oldest() {
        let mut e = egress();
        e.push(udp(40000, 1), T0);
        e.push(udp(40000, 2), T0);
        for i in 0..LIMIT {
            e.push(icmp(i as u8), T0);
        }
        assert_eq!(e.len(), LIMIT);
        assert_eq!(e.dropped, 2);
        assert_eq!(e.most, LIMIT, "no more than the limit ever waits");
        let tags: Vec<u8> = std::iter::from_fn(|| e.pop(T0)).filter(|f| f[14 + 9] == 17).map(|f| f[42]).collect();
        assert_eq!(tags, [1, 2], "the small flow lost nothing");
    }

    /// Frames of one flow leave in the order they were made.
    #[test]
    fn one_flow_keeps_its_order() {
        let mut e = egress();
        for i in 0..10 {
            e.push(udp(1, i), T0);
        }
        let tags: Vec<u8> = std::iter::from_fn(|| e.pop(T0)).map(|f| f[42]).collect();
        assert_eq!(tags, (0..10).collect::<Vec<_>>());
        assert!(e.is_empty());
    }

    /// **A queue whose head has waited past the target for an interval drops
    /// at its head, and a queue that stays under the target never does**: a
    /// flow made twice as fast as the ring drains, against one made as fast.
    #[test]
    fn a_standing_queue_is_dropped_from_and_a_draining_one_is_not() {
        let ms = Duration::from_millis;
        let mut e = egress();
        for t in 0..2000u64 {
            e.push(segment((t % 251) as u8), ms(t));
            e.push(segment((t % 251) as u8), ms(t));
            e.pop(ms(t));
        }
        assert!(e.delay_dropped > 0, "a queue growing by a frame a millisecond was never dropped from");
        assert_eq!(e.dropped, 0, "CoDel held it under the limit");

        let mut e = egress();
        for t in 0..2000u64 {
            e.push(segment(0), ms(t));
            e.pop(ms(t)).expect("the frame just made");
        }
        assert_eq!(e.delay_dropped, 0, "a queue the ring keeps empty was dropped from");
    }

    /// The first drop comes one interval after the sojourn passed the target,
    /// and not before (RFC 8289 §4.3).
    #[test]
    fn the_first_drop_waits_an_interval_past_the_target() {
        let ms = Duration::from_millis;
        let mut e = egress();
        for i in 0..200u8 {
            e.push(segment(i), T0);
        }
        // Every head from here on has waited past the target.
        e.pop(TARGET).expect("a frame");
        let mut t = TARGET + ms(1);
        while t < TARGET + INTERVAL {
            e.pop(t).expect("a frame");
            assert_eq!(e.delay_dropped, 0, "dropped at {t:?}, inside the interval");
            t += ms(1);
        }
        e.pop(TARGET + INTERVAL).expect("a frame");
        assert_eq!(e.delay_dropped, 1, "no drop once the interval had passed");
    }
}
