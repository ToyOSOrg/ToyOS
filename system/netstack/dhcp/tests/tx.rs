//! Building and retransmitting.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_dhcp::{HostName, Phase};

#[test]
fn s_dhcp_dh_001_the_first_discover() {
    let (d, out) = D::ds(&[XID, 1_000]);
    let t = sent(&out);
    assert_eq!(t.payload, payload_of(V_DHCP_DISCOVER));
    assert_eq!((t.source, t.destination), (Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST));
    assert_eq!(framed(t), hex(V_DHCP_DISCOVER));
    assert_eq!(d.client.phase(), Phase::Selecting);
    assert_eq!(d.deadline(), Some(4_000));
}

#[test]
fn s_dhcp_dh_002_the_host_name() {
    let (_, out) = D::named(&[XID, 1_000], None);
    let with_name = payload_of(V_DHCP_DISCOVER);
    let name = [0x0c, 5, b't', b'o', b'y', b'o', b's'];
    let at = with_name.windows(7).position(|w| w == name).unwrap();
    let mut expected = [&with_name[..at], &with_name[at + 7..]].concat();
    expected.extend_from_slice(&[0; 7]);
    assert_eq!(sent(&out).payload, expected);
    for bad in ["", &"a".repeat(64), "a_b", "-ab", "ab-"] {
        assert!(HostName::new(bad).is_none(), "{bad:?}");
    }
    for good in ["toyos-t14", "9lives"] {
        assert!(HostName::new(good).is_some(), "{good}");
    }
}

#[test]
fn s_dhcp_dh_003_the_discover_schedule() {
    let (mut d, out) = D::ds(&[XID, 1_000, 0, 2_000, 1_000, 1_000, 1_000]);
    let mut sent_at = vec![(0, xid_secs(sent(&out)))];
    sent_at.extend(d.run(188_000).iter().map(|(t, o)| (*t, xid_secs(sent(o)))));
    let times: Vec<u64> = sent_at.iter().map(|(t, _)| *t).collect();
    assert_eq!(times, [0, 4_000, 11_000, 28_000, 60_000, 124_000, 188_000]);
    assert!(sent_at.iter().all(|(_, (xid, _))| *xid == XID));
    let secs: Vec<u16> = sent_at.iter().map(|(_, (_, s))| *s).collect();
    assert_eq!(secs, [0, 4, 11, 28, 60, 124, 188]);
}

#[test]
fn s_dhcp_dh_004_jitter_offsets() {
    for (draw, deadline) in [(0, 3_000), (1_000, 4_000), (2_000, 5_000), (2_001, 3_000), (0xffff_ffff, 3_885)] {
        let (d, _) = D::ds(&[XID, draw]);
        assert_eq!(d.deadline(), Some(deadline), "draw {draw}");
    }
}

#[test]
fn s_dhcp_dh_005_secs_saturates() {
    let (mut d, _) = D::ds(&[XID, 1_000]);
    let out = d.timer(70_000_000);
    assert_eq!(xid_secs(sent(&out)).1, 65_535);
}

#[test]
fn s_dhcp_dh_006_the_request() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    let out = d.receive(10, &offer());
    let t = sent(&out);
    assert_eq!(t.payload, payload_of(V_DHCP_REQUEST));
    assert_eq!(framed(t), hex(V_DHCP_REQUEST));
    assert_eq!(d.client.phase(), Phase::Requesting);
    assert_eq!(d.deadline(), Some(4_010));
}

#[test]
fn s_dhcp_dh_008_the_request_repeats_the_discovers_secs() {
    let (mut d, _) = D::ds(&[XID, 1_000]);
    let retransmitted = d.timer(4_000);
    assert_eq!(xid_secs(sent(&retransmitted)).1, 4);
    let request = d.receive(4_500, &offer());
    assert_eq!(xid_secs(sent(&request)).1, 4);
    let again: Vec<(u64, u16)> = d.run(32_500).iter().map(|(t, o)| (*t, xid_secs(sent(o)).1)).collect();
    assert_eq!(again, [(8_500, 4), (16_500, 4), (32_500, 4)]);
}

#[test]
fn s_dhcp_dh_009_four_requests_then_start_over() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    let first = d.receive(10, &offer());
    assert_eq!(xid_secs(sent(&first)), (XID, 0));
    let requests = d.run(28_010);
    assert_eq!(requests.iter().map(|(t, _)| *t).collect::<Vec<_>>(), [4_010, 12_010, 28_010]);
    assert!(requests.iter().all(|(_, o)| xid_secs(sent(o)) == (XID, 0) && message_type(sent(o)) == 3));
    d.draws.extend([0x1111_1111, 1_000]);
    let out = d.timer(60_010);
    assert_eq!(d.count(toyos_dhcp::Counter::RequestTimeout), 1);
    assert_eq!(d.logged(toyos_dhcp::Counter::RequestTimeout), 1);
    let t = sent(&out);
    assert_eq!((message_type(t), xid_secs(t)), (1, (0x1111_1111, 0)));
    assert_eq!(d.client.phase(), Phase::Selecting);
    assert_eq!(d.deadline(), Some(64_010));
}

#[test]
fn s_dhcp_dh_010_a_slow_offer_still_counts() {
    let (mut d, _) = D::ds(&[XID, 1_000]);
    let again = d.timer(4_000);
    assert_eq!(xid_secs(sent(&again)).0, XID);
    let out = d.receive(4_100, &offer());
    assert_eq!(message_type(sent(&out)), 3);
}
