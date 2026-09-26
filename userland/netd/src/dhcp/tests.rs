//! The client against a server played here. Every frame the client sends is
//! read back by Fuchsia's `packet-formats`, which checks both checksums and
//! every length on its own terms, and every server frame is written by it: the
//! client's framing is judged by a second implementation, not by itself.

use super::*;

use std::num::NonZeroU16;

use net_types::ethernet::Mac;
use net_types::ip::Ipv4Addr;
use packet::{Buf, NestableSerializer, NoOpSerializationContext, ParseBuffer, Serializer};
use packet_formats::ethernet::{EtherType, EthernetFrame, EthernetFrameBuilder, EthernetFrameLengthCheck};
use packet_formats::ip::{IpPacket, IpProto, Ipv4Proto};
use packet_formats::ipv4::{Ipv4Packet, Ipv4PacketBuilder};
use packet_formats::udp::{UdpPacket, UdpPacketBuilder, UdpParseArgs};

const MAC: [u8; 6] = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const SERVER_MAC: [u8; 6] = [0x52, 0x55, 10, 0, 2, 2];
const SERVER: [u8; 4] = [10, 0, 2, 2];
const OFFERED: [u8; 4] = [10, 0, 2, 15];

/// A random source the tests can predict: every transaction id and every
/// jitter is this sequence.
fn draw() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0x1234_5678);
    NEXT.fetch_add(0x9e37_79b9, Ordering::Relaxed)
}

/// What a frame the client sent says, as `packet-formats` reads it.
#[derive(Debug)]
struct Sent {
    to_mac: [u8; 6],
    from: [u8; 4],
    to: [u8; 4],
    xid: u32,
    ciaddr: [u8; 4],
    options: Vec<(u8, Vec<u8>)>,
}

impl Sent {
    fn option(&self, code: u8) -> Option<&[u8]> {
        self.options.iter().find(|(c, _)| *c == code).map(|(_, d)| d.as_slice())
    }

    fn kind(&self) -> u8 {
        self.option(OPT_MESSAGE_TYPE).expect("every message names its type")[0]
    }
}

fn read(frame: &[u8]) -> Sent {
    let mut buf = frame;
    let eth = buf.parse_with::<_, EthernetFrame<_>>(EthernetFrameLengthCheck::NoCheck).expect("an Ethernet frame");
    assert_eq!(eth.src_mac(), Mac::new(MAC));
    assert_eq!(eth.ethertype(), Some(EtherType::Ipv4));
    let to_mac = eth.dst_mac().bytes();
    let mut body = eth.body();
    let ip = body.parse::<Ipv4Packet<_>>().expect("an IPv4 packet whose header checksum holds");
    let (from, to) = (ip.src_ip().ipv4_bytes(), ip.dst_ip().ipv4_bytes());
    let mut body = ip.body();
    let udp = body
        .parse_with::<_, UdpPacket<_>>(UdpParseArgs::new(ip.src_ip(), ip.dst_ip()))
        .expect("a UDP datagram whose checksum holds");
    assert_eq!(udp.src_port().map(NonZeroU16::get), Some(CLIENT_PORT));
    assert_eq!(udp.dst_port().get(), SERVER_PORT);
    let m = udp.body();
    assert_eq!((m[0], m[1], m[2]), (1, 1, 6), "a BOOTREQUEST over Ethernet");
    assert_eq!(&m[28..34], &MAC, "chaddr");
    assert_eq!(&m[236..240], &MAGIC);
    let mut options = Vec::new();
    let mut rest = &m[240..];
    loop {
        let (&code, after) = rest.split_first().expect("the options end");
        if code == OPT_END {
            break;
        }
        let (&len, after) = after.split_first().expect("a length");
        options.push((code, after[..usize::from(len)].to_vec()));
        rest = &after[usize::from(len)..];
    }
    Sent {
        to_mac,
        from,
        to,
        xid: u32::from_be_bytes(m[4..8].try_into().unwrap()),
        ciaddr: m[12..16].try_into().unwrap(),
        options,
    }
}

/// A server's reply, written by `packet-formats`.
fn reply(kind: u8, xid: u32, yiaddr: [u8; 4], options: &[(u8, &[u8])]) -> Vec<u8> {
    reply_as(kind, xid, yiaddr, MAC, CLIENT_PORT, options)
}

/// The same, to `chaddr` on `port`.
fn reply_as(kind: u8, xid: u32, yiaddr: [u8; 4], chaddr: [u8; 6], port: u16, options: &[(u8, &[u8])]) -> Vec<u8> {
    let mut m = vec![0u8; 236];
    m[0] = 2;
    m[1] = 1;
    m[2] = 6;
    m[4..8].copy_from_slice(&xid.to_be_bytes());
    m[16..20].copy_from_slice(&yiaddr);
    m[28..34].copy_from_slice(&chaddr);
    m.extend_from_slice(&MAGIC);
    m.extend_from_slice(&[OPT_MESSAGE_TYPE, 1, kind]);
    for (code, data) in options {
        m.push(*code);
        m.push(data.len() as u8);
        m.extend_from_slice(data);
    }
    m.push(OPT_END);
    let src = Ipv4Addr::new(SERVER);
    let dst = Ipv4Addr::new([255; 4]);
    Buf::new(m, ..)
        .wrap_in(UdpPacketBuilder::new(src, dst, NonZeroU16::new(SERVER_PORT), NonZeroU16::new(port).unwrap()))
        .wrap_in(Ipv4PacketBuilder::new(src, dst, 64, Ipv4Proto::Proto(IpProto::Udp)))
        .wrap_in(EthernetFrameBuilder::new(Mac::new(SERVER_MAC), Mac::new([0xff; 6]), EtherType::Ipv4, 0))
        .serialize_vec_outer(&mut NoOpSerializationContext)
        .expect("serialized")
        .unwrap_b()
        .into_inner()
}

const LEASE_SECS: u32 = 3600;

fn offer(xid: u32) -> Vec<u8> {
    reply(OFFER, xid, OFFERED, &[(OPT_SERVER_ID, &SERVER), (OPT_SUBNET_MASK, &[255, 255, 255, 0])])
}

fn ack(xid: u32, yiaddr: [u8; 4]) -> Vec<u8> {
    reply(
        ACK,
        xid,
        yiaddr,
        &[
            (OPT_SERVER_ID, &SERVER),
            (OPT_SUBNET_MASK, &[255, 255, 255, 0]),
            (OPT_ROUTER, &SERVER),
            (OPT_DNS, &[10, 0, 2, 3, 10, 0, 2, 4, 10, 0, 2, 5, 10, 0, 2, 6]),
            (OPT_LEASE_TIME, &LEASE_SECS.to_be_bytes()),
        ],
    )
}

/// A client bound to a lease at `t0`, and the moment it was bound.
fn bound() -> (Client, Instant) {
    let t0 = Instant::now();
    let mut c = Client::new(MAC, t0, draw);
    assert_eq!(c.on_time(t0), None);
    let discover = read(&c.take_frames()[0]);
    assert_eq!(c.on_frame(&offer(discover.xid), t0), None);
    let request = read(&c.take_frames()[0]);
    let Some(Change::Leased(_)) = c.on_frame(&ack(request.xid, OFFERED), t0) else { panic!("no lease") };
    (c, t0)
}

/// **The four messages of RFC 2131 §3.1, each read back by a second
/// implementation**: a broadcast DISCOVER from 0.0.0.0, a REQUEST naming the
/// offer and its server under the same transaction, and a lease with every
/// field the ACK carried — the first three resolvers of four.
#[test]
fn a_lease_takes_discover_offer_request_ack() {
    let t0 = Instant::now();
    let mut c = Client::new(MAC, t0, draw);
    assert_eq!(c.wake_at(), t0, "the first DISCOVER is owed at once");
    assert_eq!(c.on_time(t0), None);
    let frames = c.take_frames();
    assert_eq!(frames.len(), 1);
    let discover = read(&frames[0]);
    assert_eq!(discover.kind(), DISCOVER);
    assert_eq!((discover.to_mac, discover.from, discover.to), ([0xff; 6], [0; 4], [255; 4]));
    assert_eq!(discover.option(OPT_HOST_NAME), Some(HOSTNAME.as_bytes()));

    assert_eq!(c.on_frame(&offer(discover.xid), t0), None);
    let request = read(&c.take_frames()[0]);
    assert_eq!(request.kind(), REQUEST);
    assert_eq!(request.xid, discover.xid);
    assert_eq!(request.option(OPT_REQUESTED_ADDRESS), Some(&OFFERED[..]));
    assert_eq!(request.option(OPT_SERVER_ID), Some(&SERVER[..]));
    assert_eq!(request.ciaddr, [0; 4]);

    let Some(Change::Leased(lease)) = c.on_frame(&ack(request.xid, OFFERED), t0) else {
        panic!("the ACK granted no lease")
    };
    assert_eq!((lease.address, lease.prefix, lease.router, lease.server), (OFFERED, 24, Some(SERVER), SERVER));
    assert_eq!(lease.dns, vec![[10, 0, 2, 3], [10, 0, 2, 4], [10, 0, 2, 5]]);
    assert_eq!(c.lease(), Some(&lease));
    assert_eq!(c.wake_at(), t0 + Duration::from_secs(u64::from(LEASE_SECS) / 2), "T1 defaults to half the lease");
}

/// **An unanswered DISCOVER is sent again after 4 s, then 8, 16, 32 and 64,
/// each within ±1 s** (RFC 2131 §4.1), under the one transaction.
#[test]
fn an_unanswered_discover_backs_off_to_sixty_four_seconds() {
    let t0 = Instant::now();
    let mut c = Client::new(MAC, t0, draw);
    c.on_time(t0);
    let xid = read(&c.take_frames()[0]).xid;
    let mut at = t0;
    for wait in [4u64, 8, 16, 32, 64] {
        let due = c.wake_at();
        let waited = due - at;
        assert!(
            waited >= Duration::from_secs(wait - 1) && waited <= Duration::from_secs(wait + 1),
            "retransmission after {waited:?}, not {wait} s ± 1 s"
        );
        assert_eq!(c.on_time(due - Duration::from_millis(1)), None);
        assert!(c.take_frames().is_empty(), "nothing is sent before it is due");
        c.on_time(due);
        let again = read(&c.take_frames()[0]);
        assert_eq!((again.kind(), again.xid), (DISCOVER, xid));
        at = due;
    }
}

/// **A reply to another transaction, to another client, or whose UDP
/// checksum does not hold is read as nothing.**
#[test]
fn a_reply_that_is_not_this_exchanges_is_ignored() {
    let t0 = Instant::now();
    let mut c = Client::new(MAC, t0, draw);
    c.on_time(t0);
    let xid = read(&c.take_frames()[0]).xid;
    assert_eq!(c.on_frame(&offer(xid.wrapping_add(1)), t0), None);
    let server_id: &[u8] = &SERVER;
    let other = reply_as(OFFER, xid, OFFERED, [2, 0, 0, 0, 0, 9], CLIENT_PORT, &[(OPT_SERVER_ID, server_id)]);
    assert_eq!(c.on_frame(&other, t0), None);
    let mut corrupt = offer(xid);
    let last = corrupt.len() - 2;
    corrupt[last] ^= 0xff;
    assert_eq!(c.on_frame(&corrupt, t0), None);
    assert!(c.take_frames().is_empty(), "no REQUEST answered a reply that was not this exchange's");
    assert_eq!(c.on_frame(&offer(xid), t0), None);
    assert_eq!(read(&c.take_frames()[0]).kind(), REQUEST);
}

/// **An ACK with no subnet mask configures nothing and is ignored**, and a
/// NAK sends the client back to discovery.
#[test]
fn an_ack_without_a_mask_is_ignored_and_a_nak_starts_over() {
    let t0 = Instant::now();
    let mut c = Client::new(MAC, t0, draw);
    c.on_time(t0);
    let xid = read(&c.take_frames()[0]).xid;
    c.on_frame(&offer(xid), t0);
    let xid = read(&c.take_frames()[0]).xid;
    let maskless = reply(ACK, xid, OFFERED, &[(OPT_SERVER_ID, &SERVER), (OPT_LEASE_TIME, &LEASE_SECS.to_be_bytes())]);
    assert_eq!(c.on_frame(&maskless, t0), None);
    assert!(c.lease().is_none());
    let nak = reply(NAK, xid, [0; 4], &[(OPT_SERVER_ID, &SERVER)]);
    assert_eq!(c.on_frame(&nak, t0), None);
    assert_eq!(c.wake_at(), t0, "discovery is owed again at once");
    c.on_time(t0);
    let again = read(&c.take_frames()[0]);
    assert_eq!(again.kind(), DISCOVER);
    assert_ne!(again.xid, xid, "a new exchange takes a new transaction id");
}

/// **At T1 the client asks the lease's own server, unicast, from the address
/// it holds; at T2 it asks anyone; at the lease's end it has none** — and
/// starts over (RFC 2131 §4.4.5).
#[test]
fn a_lease_renews_at_t1_rebinds_at_t2_and_is_lost_at_its_end() {
    let (mut c, t0) = bound();
    let t1 = t0 + Duration::from_secs(u64::from(LEASE_SECS) / 2);
    let t2 = t0 + Duration::from_secs(u64::from(LEASE_SECS) * 7 / 8);
    let end = t0 + Duration::from_secs(u64::from(LEASE_SECS));
    assert_eq!(c.on_time(t1), None);
    let renew = read(&c.take_frames()[0]);
    assert_eq!(renew.kind(), REQUEST);
    assert_eq!((renew.to_mac, renew.from, renew.to, renew.ciaddr), (SERVER_MAC, OFFERED, SERVER, OFFERED));
    assert_eq!(renew.option(OPT_REQUESTED_ADDRESS), None, "RENEWING names the address in ciaddr alone");
    assert_eq!(renew.option(OPT_SERVER_ID), None);
    assert!(c.lease().is_some(), "the lease is held while it is renewed");

    let mut at = t1;
    while c.wake_at() < t2 {
        let next = c.wake_at();
        assert!(next - at >= RENEW_FLOOR || next == t2, "RENEWING asked again after {:?}", next - at);
        c.on_time(next);
        assert_eq!(read(&c.take_frames()[0]).to, SERVER);
        at = next;
    }
    assert_eq!(c.on_time(t2), None);
    let rebind = read(&c.take_frames()[0]);
    assert_eq!((rebind.to_mac, rebind.to, rebind.ciaddr), ([0xff; 6], [255; 4], OFFERED));
    while c.wake_at() < end {
        let next = c.wake_at();
        assert_eq!(c.on_time(next), None);
        c.take_frames();
    }
    assert_eq!(c.on_time(end), Some(Change::Lost));
    assert!(c.lease().is_none());
    assert_eq!(read(&c.take_frames()[0]).kind(), DISCOVER, "discovery starts at once");
}

/// **An ACK while renewing extends the lease from the moment it came**, and a
/// NAK loses it.
#[test]
fn a_renewal_acked_extends_the_lease_and_one_naked_loses_it() {
    let (mut c, t0) = bound();
    let t1 = t0 + Duration::from_secs(u64::from(LEASE_SECS) / 2);
    c.on_time(t1);
    let xid = read(&c.take_frames()[0]).xid;
    let Some(Change::Leased(_)) = c.on_frame(&ack(xid, OFFERED), t1) else { panic!("the renewal granted nothing") };
    assert_eq!(c.wake_at(), t1 + Duration::from_secs(u64::from(LEASE_SECS) / 2));
    let t1 = c.wake_at();
    c.on_time(t1);
    let xid = read(&c.take_frames()[0]).xid;
    let nak = reply(NAK, xid, [0; 4], &[(OPT_SERVER_ID, &SERVER)]);
    assert_eq!(c.on_frame(&nak, t1), Some(Change::Lost));
    assert!(c.lease().is_none());
}

/// Only port 68 is the client's.
#[test]
fn the_client_takes_port_sixty_eight_alone() {
    let frame = offer(1);
    assert!(Client::takes(&frame));
    let server_id: &[u8] = &SERVER;
    let other = reply_as(OFFER, 1, OFFERED, MAC, 69, &[(OPT_SERVER_ID, server_id)]);
    assert!(!Client::takes(&other));
}
