//! The frames the stack has made and the transmit ring has no slot for yet.
//!
//! **One queue per flow, served in turn, and the fattest pays for an
//! overload** — RFC 8290's flow queuing (§4.1) without CoDel's delay control:
//! a frame joins its flow's queue, the ring takes one frame from each flow
//! with frames waiting in turn, and a push that takes the total past
//! [`LIMIT`] drops the head of whichever flow holds the most (§4.1.1). So a
//! flow that makes frames faster than the ring drains them — a ping flood's
//! replies, one upload on a slow link — queues behind itself: a lookup's
//! query waits at most one frame per other flow, and a flood can never hold
//! more of the ring than its share.
//!
//! A flow is what [`Flow::of`] reads off the frame's own headers; a frame the
//! reading does not recognise is its EtherType's flow.

use std::collections::{HashMap, VecDeque};

/// How many frames may wait across every flow before the fattest flow's
/// oldest is dropped: a thousand full frames, about 1.5 MiB. Linux's
/// `fq_codel` defaults to ten times that, for a link that drains ten times
/// faster than this machine's slowest ring.
pub const LIMIT: usize = 1024;

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

/// The waiting frames, and what `inspect` reads about them.
#[derive(Default)]
pub struct Egress {
    flows: HashMap<Flow, VecDeque<Vec<u8>>>,
    /// The flows holding frames, in the order the ring serves them.
    turn: VecDeque<Flow>,
    /// Frames waiting across every flow.
    waiting: usize,
    /// The most that have waited at once.
    pub most: usize,
    /// Frames dropped at [`LIMIT`].
    pub dropped: u64,
}

impl Egress {
    /// Queue `frame` behind its flow's, and drop the fattest flow's oldest if
    /// that takes the total past [`LIMIT`].
    pub fn push(&mut self, frame: Vec<u8>) {
        let flow = Flow::of(&frame);
        let queue = self.flows.entry(flow).or_default();
        if queue.is_empty() {
            self.turn.push_back(flow);
        }
        queue.push_back(frame);
        self.waiting += 1;
        self.most = self.most.max(self.waiting);
        if self.waiting > LIMIT {
            let fattest = *self
                .flows
                .iter()
                .max_by_key(|(_, q)| q.len())
                .map(|(flow, _)| flow)
                .expect("frames are waiting");
            self.take(fattest).expect("the fattest flow holds a frame");
            self.dropped += 1;
        }
    }

    /// The next frame in turn: the oldest of the flow at the head of the turn,
    /// which goes to the back of it while it holds more.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let flow = self.turn.pop_front()?;
        let frame = self.take(flow).expect("a flow in turn holds a frame");
        if self.flows.contains_key(&flow) {
            self.turn.push_back(flow);
        }
        Some(frame)
    }

    /// Whether any frame waits.
    pub fn is_empty(&self) -> bool {
        self.waiting == 0
    }

    /// How many frames wait now.
    pub fn len(&self) -> usize {
        self.waiting
    }

    /// Take `flow`'s oldest frame, and forget the flow once it holds none.
    fn take(&mut self, flow: Flow) -> Option<Vec<u8>> {
        let queue = self.flows.get_mut(&flow)?;
        let frame = queue.pop_front()?;
        if queue.is_empty() {
            self.flows.remove(&flow);
            self.turn.retain(|f| *f != flow);
        }
        self.waiting -= 1;
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

    /// **A flow queued behind a flood leaves before the flood's backlog**:
    /// the ring serves one frame a flow in turn.
    #[test]
    fn a_flow_behind_a_flood_waits_one_turn() {
        let mut e = Egress::default();
        for i in 0..100 {
            e.push(icmp(i));
        }
        e.push(udp(40000, 7));
        assert_eq!(e.pop().unwrap()[42], 0, "the flood's first");
        assert_eq!(e.pop().unwrap()[42], 7, "the query, next in turn");
        assert_eq!(e.pop().unwrap()[42], 1, "then the flood again");
        assert_eq!(e.len(), 98);
    }

    /// **Past the limit the fattest flow's oldest frame goes**, and a small
    /// flow keeps every frame.
    #[test]
    fn an_overload_drops_the_fattest_flows_oldest() {
        let mut e = Egress::default();
        e.push(udp(40000, 1));
        e.push(udp(40000, 2));
        for i in 0..LIMIT {
            e.push(icmp(i as u8));
        }
        assert_eq!(e.len(), LIMIT);
        assert_eq!(e.dropped, 2);
        assert_eq!(e.most, LIMIT + 1);
        let tags: Vec<u8> = std::iter::from_fn(|| e.pop()).filter(|f| f[14 + 9] == 17).map(|f| f[42]).collect();
        assert_eq!(tags, [1, 2], "the small flow lost nothing");
    }

    /// Frames of one flow leave in the order they were made.
    #[test]
    fn one_flow_keeps_its_order() {
        let mut e = Egress::default();
        for i in 0..10 {
            e.push(udp(1, i));
        }
        let tags: Vec<u8> = std::iter::from_fn(|| e.pop()).map(|f| f[42]).collect();
        assert_eq!(tags, (0..10).collect::<Vec<_>>());
        assert!(e.is_empty());
    }
}
