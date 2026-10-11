//! DH-73 and DH-74: invariants over seeded random runs.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_dhcp::{Config, Counter, HostName, Output, Transmission};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Refusals of a whole message: a refused message names exactly one, an accepted one none.
const REFUSALS: [Counter; 27] = [
    Counter::Truncated,
    Counter::NotReply,
    Counter::HardwareType,
    Counter::ChaddrMismatch,
    Counter::BootpReply,
    Counter::OptionTruncated,
    Counter::OverloadInvalid,
    Counter::OptionLength,
    Counter::MessageTypeMissing,
    Counter::WrongDirection,
    Counter::Forcerenew,
    Counter::MessageTypeUnsupported,
    Counter::XidMismatch,
    Counter::ClientIdMismatch,
    Counter::NoServerId,
    Counter::ServerIdInvalid,
    Counter::UnexpectedOffer,
    Counter::UnexpectedAck,
    Counter::UnexpectedNak,
    Counter::YiaddrInvalid,
    Counter::NoLeaseTime,
    Counter::LeaseZero,
    Counter::NoSubnetMask,
    Counter::MaskInvalid,
    Counter::AckWrongServer,
    Counter::AckAddressChanged,
    Counter::NakWrongServer,
];

fn refusals(d: &D) -> u64 {
    REFUSALS.iter().map(|&c| d.count(c)).sum()
}

/// A server message for the client's transaction or another, sometimes mutated.
fn message(rng: &mut Rng, xid: u32) -> Vec<u8> {
    let xid = if rng.below(4) == 0 { rng.next() as u32 } else { xid };
    let mut m = match rng.below(4) {
        0 => server(xid, A, &offer_options()),
        1 => server(xid, A, &ack_options()),
        2 => nak(xid, R),
        _ => server(xid, A, &with(ack_options(), 80, Some(vec![]))),
    };
    match rng.below(5) {
        0 => {
            let at = rng.below(m.len() as u64) as usize;
            m[at] ^= 1 << rng.below(8);
        }
        1 => m.truncate(rng.below(m.len() as u64) as usize),
        _ => {}
    }
    m
}

struct Seen {
    transmissions: Vec<Transmission>,
    configured: usize,
}

/// One seeded run; checks DH-73's invariants as it goes and keeps every transmission.
fn run(seed: u64, host: Option<HostName>) -> Seen {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let (mut d, first) = D::named(&[], host);
    d.draws = (0..10_000).map(|_| rng.next() as u32).collect();
    let mut seen = Seen { transmissions: first.transmit.into_iter().collect(), configured: 0 };
    let mut configured: Option<Ipv4Addr> = None;
    let mut now = 0u64;
    for _ in 0..600 {
        let span = if rng.below(3) == 0 { 2_000_000 } else { 30_000 };
        now += rng.below(span);
        let due = d.client.next_deadline().is_some_and(|t| t <= at(now));
        let before = refusals(&d);
        let (out, verified_call): (Output, bool) = match rng.below(8) {
            _ if due => (d.timer(if rng.below(2) == 0 { now } else { d.client.next_deadline().unwrap().nanos().div_ceil(1_000_000) }), false),
            0 => (d.verified(now), true),
            1 => (d.conflict(now, MAC_B), false),
            2 => (d.not_verified(now), false),
            3 => (d.link_up(now), false),
            _ => {
                let m = message(&mut rng, d.client.xid().unwrap_or(0));
                let phase = d.client.phase();
                let out = d.receive(now, &m);
                let refused = out == Output::default() && d.client.phase() == phase;
                assert_eq!(refusals(&d) - before, u64::from(refused), "seed {seed}: a refused message names exactly one reason");
                (out, false)
            }
        };
        now = d.now;
        if let Some(Config::Configured(_)) = &out.config {
            seen.configured += 1;
            assert!(verified_call, "seed {seed}: configured without verified");
        }
        match &out.config {
            Some(Config::Configured(l) | Config::Extended(l) | Config::Reconfigured(l)) => configured = Some(l.address),
            Some(Config::Deconfigured(_)) => configured = None,
            None => {}
        }
        if let Some(t) = &out.transmit {
            let kind = message_type(t);
            if t.source.is_unspecified() {
                let ciaddr = Ipv4Addr::new(t.payload[12], t.payload[13], t.payload[14], t.payload[15]);
                assert!(matches!(kind, 1 | 3 | 4), "seed {seed}");
                assert!(ciaddr.is_unspecified(), "seed {seed}: only RENEWING and REBINDING send from the lease");
            } else {
                assert_eq!(Some(t.source), configured, "seed {seed}: sent from an address not configured");
            }
            seen.transmissions.push(t.clone());
        }
        if let Some(deadline) = d.client.next_deadline() {
            assert!(deadline > at(d.now), "seed {seed}: a deadline not after now");
        }
    }
    seen
}

#[test]
fn s_dhcp_dh_073_prop_invariants() {
    let (mut configured, mut from_lease) = (0, 0);
    for seed in 1..=64 {
        let seen = run(seed, HostName::new("toyos"));
        configured += seen.configured;
        from_lease += seen.transmissions.iter().filter(|t| !t.source.is_unspecified()).count();
    }
    assert!(configured > 50 && from_lease > 50, "the runs reach leases: {configured} configured, {from_lease} sent from one");
}

/// Walks a built message's options: each code and its data, until END.
fn options(payload: &[u8]) -> (Vec<(u8, Vec<u8>)>, usize) {
    let mut at = 240;
    let mut found = Vec::new();
    while payload[at] != 255 {
        let (code, len) = (payload[at], usize::from(payload[at + 1]));
        found.push((code, payload[at + 2..at + 2 + len].to_vec()));
        at += 2 + len;
    }
    (found, at)
}

#[test]
fn s_dhcp_dh_074_prop_every_message_is_built_one_way() {
    const ORDER: [u8; 8] = [53, 54, 50, 61, 57, 55, 12, 80];
    for (seed, host) in (1..=32).map(|s| (s, if s % 2 == 0 { HostName::new("toyos") } else { None })) {
        let named = host.is_some();
        for t in run(seed, host).transmissions {
            let p = &t.payload;
            assert_eq!(p.len(), 300, "seed {seed}");
            let (found, end) = options(p);
            assert!(p[end + 1..].iter().all(|b| *b == 0), "zero padding");
            let codes: Vec<u8> = found.iter().map(|(c, _)| *c).collect();
            let order: Vec<u8> = ORDER.iter().copied().filter(|c| codes.contains(c)).collect();
            assert_eq!(codes, order, "seed {seed}: `ORDER`'s order");
            let kind = found[0].1[0];
            let ciaddr = Ipv4Addr::new(p[12], p[13], p[14], p[15]);
            let expected: Vec<u8> = match (kind, codes.contains(&54), codes.contains(&50), ciaddr.is_unspecified()) {
                (1, ..) => vec![53, 61, 57, 55, 12, 80],
                (3, true, true, true) => vec![53, 54, 50, 61, 57, 55, 12],
                (3, false, true, true) => vec![53, 50, 61, 57, 55, 12],
                (3, false, false, false) => vec![53, 61, 57, 55, 12],
                (4, true, true, true) => vec![53, 54, 50, 61],
                other => panic!("seed {seed}: {other:?}"),
            };
            let expected: Vec<u8> = expected.into_iter().filter(|c| *c != 12 || named).collect();
            assert_eq!(codes, expected, "seed {seed}: the options of type {kind}");
            for (code, data) in &found {
                match code {
                    61 => assert_eq!(data, &CLIENT_ID),
                    57 => assert_eq!(data, &[5, 0xc0]),
                    55 => assert_eq!(data, &[1, 3, 6, 51, 58, 59]),
                    12 => assert_eq!(data, b"toyos"),
                    80 => assert!(data.is_empty()),
                    _ => {}
                }
            }
        }
    }
}
