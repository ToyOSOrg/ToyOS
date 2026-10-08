//! Slirp's DHCP server, replayed: the OFFER and the ACK QEMU's user network sent a ToyOS guest,
//! as `-object filter-dump` recorded them on the guest's netdev. A server nobody here wrote; its
//! addresses are slirp's defaults. What the lease must be is read off the bytes by RFC 2131 §2
//! and RFC 2132 §3.3, §3.5, §8.3, §9.2 and §9.7: 10.0.2.15 under 255.255.255.0, router and server
//! 10.0.2.2, resolver 10.0.2.3, for 86,400 s.
//!
//! The guest that was recorded drew a new transaction id for its REQUEST; ToyOS's client keeps
//! the DISCOVER's (RFC 2131 §4.4.1, table 5). So each message is replayed whole into a node whose
//! draw is that message's id, and the node that takes the ACK is brought to its REQUEST by the
//! OFFER under the ACK's id.

mod common;

use std::net::Ipv4Addr;
use std::time::Duration;

use common::*;
use toyos_net_ip::AddrState;
use toyos_net_wire::Instant;

/// QEMU's default guest MAC.
const GUEST: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
const LEASED: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
const SERVER: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
const RESOLVER: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);
const OFFER_XID: u32 = 0x7d5faa2a;
const ACK_XID: u32 = 0x45ce419e;
/// Where a frame's fields are: the UDP checksum, the DHCP transaction id, and the ACK's subnet
/// mask.
const UDP_CHECKSUM: std::ops::Range<usize> = 40..42;
const XID: std::ops::Range<usize> = 46..50;
const ACK_MASK: std::ops::Range<usize> = 293..297;

const SLIRP_OFFER: &str = "
ffffffffffff52550a0002020800451002400000000040116c9c0a000202ffff
ffff00430044022c93e7020106007d5faa2a00000000000000000a00020f0a00
0202000000005254001234560000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
000000000000000000000000000000000000000000006382536335010236040a
0002020104ffffff0003040a00020206040a000203330400015180ff00000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000
";

const SLIRP_ACK: &str = "
ffffffffffff52550a0002020800451002400001000040116c9b0a000202ffff
ffff00430044022c31050201060045ce419e00000000000000000a00020f0a00
0202000000005254001234560000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
000000000000000000000000000000000000000000006382536335010536040a
0002020104ffffff0003040a00020206040a000203330400015180ff00000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000000000000000000000000000000000000000
0000000000000000000000000000
";

/// The guest's node, its link up and its DISCOVER sent under `id`.
fn discovering(id: u32) -> Wire {
    let mut wire = Wire::at(GUEST);
    wire.seed(id);
    wire.link(true);
    assert_eq!(xid(wire.last(DISCOVER)), id);
    wire
}

/// The node waiting for the ACK of the REQUEST it sent under the ACK's id: the recorded OFFER
/// brought it there, restamped with that id and its UDP checksum cleared (RFC 768: none).
fn requesting() -> Wire {
    let mut wire = discovering(ACK_XID);
    let mut offer = hex(SLIRP_OFFER);
    offer[XID].copy_from_slice(&ACK_XID.to_be_bytes());
    offer[UDP_CHECKSUM].fill(0);
    wire.deliver(&offer);
    assert_eq!(xid(wire.last(REQUEST)), ACK_XID);
    wire
}

#[test]
fn slirps_offer_is_answered_with_a_request_for_what_it_offered() {
    let mut wire = discovering(OFFER_XID);
    wire.deliver(&hex(SLIRP_OFFER));
    let request = wire.last(REQUEST);
    assert_eq!((xid(request), option(request, 50), option(request, 54)), (OFFER_XID, Some(&LEASED.octets()[..]), Some(&SERVER.octets()[..])));
}

#[test]
fn slirps_ack_is_the_lease_its_bytes_name() {
    let mut wire = requesting();
    let requested = wire.now;
    wire.deliver(&hex(SLIRP_ACK));
    assert_eq!((wire.node.lease(), wire.address(LEASED)), (None, Some(AddrState::Tentative)));
    assert!(wire.run_until(Duration::from_secs(10), |wire| wire.node.lease().is_some()), "the lease is held");
    let lease = wire.node.lease().unwrap();
    assert_eq!((lease.address, lease.prefix_len, lease.router, lease.dns.as_slice(), lease.server), (LEASED, 24, Some(SERVER), &[RESOLVER][..], SERVER));
    let expiry: Option<Instant> = lease.timers.map(|timers| timers.expiry);
    assert_eq!(expiry, Some(requested.after(Duration::from_secs(86_400))));
    assert_eq!(wire.gateways(), [SERVER]);
    assert!(wire.sent.iter().filter(|frame| frame.is_probe(LEASED)).count() == 3 && wire.sent.iter().any(|frame| frame.is_announcement(LEASED)));
}

#[test]
fn the_lease_is_read_from_the_acks_own_mask() {
    let mut ack = hex(SLIRP_ACK);
    assert_eq!(ack[ACK_MASK], [255, 255, 255, 0]);
    ack[ACK_MASK].copy_from_slice(&[255, 255, 254, 0]);

    // Its checksum no longer its bytes', the frame is nobody's ACK.
    let mut wire = requesting();
    wire.deliver(&ack);
    assert_eq!(wire.address(LEASED), None);

    // With none (RFC 768), the changed mask is the lease's.
    ack[UDP_CHECKSUM].fill(0);
    wire.deliver(&ack);
    assert!(wire.run_until(Duration::from_secs(10), |wire| wire.node.lease().is_some()), "the lease is held");
    assert_eq!(wire.node.lease().map(|lease| lease.prefix_len), Some(23));
}
