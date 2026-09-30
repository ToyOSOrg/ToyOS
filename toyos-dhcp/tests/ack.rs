//! §D8, §D10: acknowledgement, conflict detection, decline and rapid commit.

mod common;



use common::*;
use toyos_dhcp::{AddressRequest, Config, Counter, Event, Lease, Peer, Phase, Timers};

fn requesting() -> D {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    d
}

fn probing() -> D {
    let mut d = requesting();
    d.receive(20, &ack());
    d
}

fn fixture_db_lease(base: u64) -> Lease {
    Lease {
        address: A,
        prefix_len: 24,
        router: Some(R),
        dns: vec![DNS, DNS2],
        server: R,
        base: at(base),
        timers: Some(Timers { t1: at(base + 1_800_000), t2: at(base + 3_150_000), expiry: at(base + 3_600_000) }),
    }
}

#[test]
fn s_dhcp_dh_011_an_ack_is_probed_first() {
    let mut d = requesting();
    let out = d.receive(20, &ack());
    assert_eq!(out.address, Some(AddressRequest::Probe { address: A, prefix_len: 24 }));
    assert_eq!((out.transmit, out.config), (None, None));
    assert_eq!(d.client.phase(), Phase::Probing);
    assert_eq!(d.deadline(), None);
}

#[test]
fn s_dhcp_dh_012_verified_configures() {
    let mut d = probing();
    let out = d.verified(5_020);
    assert_eq!(out.config, Some(Config::Configured(fixture_db_lease(10))));
    assert_eq!(out.transmit, None);
    assert_eq!(d.client.phase(), Phase::Bound);
    assert_eq!(d.deadline(), Some(1_800_010));
}

#[test]
fn s_dhcp_dh_013_a_conflict_declines() {
    let mut d = probing();
    d.draws.push_back(0x0dec_11e0);
    let out = d.conflict(3_000, MAC_B);
    let t = sent(&out);
    assert_eq!(t.payload, payload_of(V_DHCP_DECLINE));
    assert_eq!(framed(t), hex(V_DHCP_DECLINE));
    assert_eq!(out.config, None);
    assert_eq!(d.count(Counter::Declined), 1);
    assert_eq!(d.events, [Event::Refused { rule: Counter::Declined, peer: Peer::Conflict { address: A, mac: MAC_B } }]);
    assert_eq!(d.client.phase(), Phase::BackingOff);
    d.draws.extend([0x2222_2222, 1_000]);
    assert!(d.timer(12_999).transmit.is_none());
    let again = d.timer(13_000);
    assert_eq!((message_type(sent(&again)), xid_secs(sent(&again)).0), (1, 0x2222_2222));
    assert_eq!(d.deadline(), Some(17_000));
}

#[test]
fn s_dhcp_dh_014_every_decline_waits_ten_seconds() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    let mut t = 0;
    for round in 0..11u32 {
        let xid = d.client.xid().unwrap();
        d.receive(t + 10, &server(xid, A, &offer_options()));
        d.receive(t + 20, &server(xid, A, &ack_options()));
        assert_eq!(d.client.phase(), Phase::Probing, "round {round}");
        let declined = d.conflict(t + 30, MAC_B);
        assert_eq!(message_type(sent(&declined)), 4);
        assert!(d.timer(t + 30 + 9_999).transmit.is_none());
        let discover = d.timer(t + 30 + 10_000);
        assert_eq!(message_type(sent(&discover)), 1, "round {round}: DISCOVER 10 s after the DECLINE");
        t += 10_030;
    }
    assert_eq!(d.count(Counter::Declined), 11);
}

#[test]
fn s_dhcp_dh_015_rapid_commit() {
    let (mut d, _) = D::ds(&[XID, 1_000]);
    let out = d.receive(10, &hex(V_DHCP_ACK_RAPID));
    assert_eq!(probe_of(&out), Some(A));
    assert_eq!(out.transmit, None);
    let configured = d.verified(5_010);
    let Some(Config::Configured(lease)) = configured.config else { panic!("{configured:?}") };
    assert_eq!((lease.server, lease.base), (R, at(0)));
    assert_eq!(lease.timers, Some(Timers { t1: at(1_800_000), t2: at(3_150_000), expiry: at(3_600_000) }));
}

#[test]
fn s_dhcp_dh_016_an_ack_without_rapid_commit_while_selecting() {
    let (mut d, _) = D::ds(&[XID, 1_000]);
    let out = d.receive(10, &ack());
    assert_eq!(out, toyos_dhcp::Output::default());
    assert_eq!(d.count(Counter::UnexpectedAck), 1);
    assert_eq!(d.client.phase(), Phase::Selecting);
}

#[test]
fn s_dhcp_dh_017_a_rapid_ack_answers_a_request_too() {
    let mut d = requesting();
    let out = d.receive(20, &hex(V_DHCP_ACK_RAPID));
    assert_eq!(probe_of(&out), Some(A));
    assert_eq!(d.client.phase(), Phase::Probing);
}

#[test]
fn s_dhcp_dh_018_the_lease_runs_from_the_first_request() {
    let mut d = requesting();
    let again = d.timer(4_010);
    assert_eq!(message_type(sent(&again)), 3);
    d.receive(4_020, &ack());
    let out = d.verified(9_020);
    let Some(Config::Configured(lease)) = out.config else { panic!() };
    assert_eq!(lease.base, at(10));
    assert_eq!(lease.timers.unwrap().t1, at(1_800_010));
}
