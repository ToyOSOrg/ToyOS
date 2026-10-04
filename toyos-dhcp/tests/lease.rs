//! Renewing, rebinding, expiry, NAKs and the lease's times.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_dhcp::{Config, Counter, Phase, Reason, Timers};

/// Fixture DB's ACK re-sent as `xid`, with options changed.
fn renewal_ack(xid: u32, change: impl FnOnce(&mut Vec<(u8, Vec<u8>)>)) -> Vec<u8> {
    let mut options = ack_options();
    change(&mut options);
    let mut m = server(xid, A, &options);
    m[12..16].copy_from_slice(&A.octets());
    m
}

fn renewing() -> D {
    let mut d = D::db();
    d.draws.push_back(0x0bad_cafe);
    d.timer(1_800_010);
    d
}

fn set(options: &mut [(u8, Vec<u8>)], code: u8, data: Vec<u8>) {
    options.iter_mut().find(|(c, _)| *c == code).unwrap().1 = data;
}

#[test]
fn s_dhcp_dh_019_the_first_renew() {
    let mut d = D::db();
    d.draws.push_back(0x0bad_cafe);
    let out = d.timer(1_800_010);
    let t = sent(&out);
    assert_eq!(t.payload, payload_of(V_DHCP_RENEW));
    assert_eq!((t.source, t.destination), (A, R));
    assert_eq!(framed(t), hex(V_DHCP_RENEW), "unicast to the server identifier, framed to the router");
    assert_eq!(d.client.phase(), Phase::Renewing);
    assert_eq!(d.deadline(), Some(2_475_010));
}

#[test]
fn s_dhcp_dh_020_renew_rebind_expire() {
    let mut d = D::db();
    d.draws.extend((1..=20u32).flat_map(|n| [0x1000 + n]));
    let out = d.run(3_600_010);
    let renews: Vec<(u64, u16)> = out.iter().filter(|(_, o)| o.transmit.as_ref().is_some_and(|t| t.destination == R)).map(|(t, o)| (*t, xid_secs(sent(o)).1)).collect();
    assert_eq!(renews, [(1_800_010, 0), (2_475_010, 675), (2_812_510, 1_012), (2_981_260, 1_181), (3_065_635, 1_265), (3_125_635, 1_325)]);
    let rebinds: Vec<(u64, u16)> = out
        .iter()
        .filter(|(_, o)| o.transmit.as_ref().is_some_and(|t| t.destination == Ipv4Addr::BROADCAST && t.source == A))
        .map(|(t, o)| (*t, xid_secs(sent(o)).1))
        .collect();
    assert_eq!(rebinds, [(3_150_010, 1_350), (3_375_010, 1_575), (3_487_510, 1_687), (3_547_510, 1_747)]);
    let xids: std::collections::BTreeSet<u32> = out.iter().filter_map(|(_, o)| o.transmit.as_ref()).map(|t| xid_secs(t).0).collect();
    assert_eq!(xids.len(), out.len(), "a fresh xid per transmission");
    let (last_at, last) = out.last().unwrap();
    assert_eq!(*last_at, 3_600_010);
    assert_eq!(last.config, Some(Config::Deconfigured(Reason::Expired)));
    assert_eq!(message_type(sent(last)), 1);
    assert_eq!(d.logged(Counter::LeaseExpired), 1);
}

#[test]
fn s_dhcp_dh_021_t2_alone_means_rebinding_at_t1() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    let mut options = ack_options();
    options.push((59, seconds(1_000)));
    d.receive(20, &server(XID, A, &options));
    let out = d.verified(5_020);
    let Some(Config::Configured(lease)) = out.config else { panic!() };
    assert_eq!(lease.timers.map(|t| (t.t1, t.t2)), Some((at(1_000_010), at(1_000_010))));
    d.draws.push_back(0x1c0f_fee5);
    let rebind = d.timer(1_000_010);
    let t = sent(&rebind);
    assert_eq!(t.payload, payload_of(V_DHCP_REBIND));
    assert_eq!(framed(t), hex(V_DHCP_REBIND));
    assert_eq!(d.client.phase(), Phase::Rebinding);
}

#[test]
fn s_dhcp_dh_022_extended() {
    let mut d = renewing();
    let out = d.receive(1_800_500, &renewal_ack(0x0bad_cafe, |_| {}));
    let Some(Config::Extended(lease)) = out.config else { panic!("{out:?}") };
    assert_eq!(lease.base, at(1_800_010));
    assert_eq!(lease.timers, Some(Timers { t1: at(3_600_010), t2: at(4_950_010), expiry: at(5_400_010) }));
    assert_eq!(d.client.phase(), Phase::Bound);
}

#[test]
fn s_dhcp_dh_023_reconfigured() {
    let mut d = renewing();
    let out = d.receive(1_800_500, &renewal_ack(0x0bad_cafe, |o| set(o, 6, ip(Ipv4Addr::new(192, 0, 2, 54)))));
    let Some(Config::Reconfigured(lease)) = out.config else { panic!("{out:?}") };
    assert_eq!((lease.dns.as_slice(), lease.address, lease.router), (&[Ipv4Addr::new(192, 0, 2, 54)][..], A, Some(R)));
}

#[test]
fn s_dhcp_dh_024_an_ack_for_another_address() {
    let mut d = renewing();
    let mut m = renewal_ack(0x0bad_cafe, |_| {});
    m[16..20].copy_from_slice(&[192, 0, 2, 7]);
    assert_eq!(d.receive(1_800_500, &m), toyos_dhcp::Output::default());
    assert_eq!(d.logged(Counter::AckAddressChanged), 1);
    assert_eq!((d.client.phase(), d.deadline()), (Phase::Renewing, Some(2_475_010)));
}

#[test]
fn s_dhcp_dh_025_an_ack_from_another_server() {
    let mut d = renewing();
    d.receive(1_800_500, &renewal_ack(0x0bad_cafe, |o| set(o, 54, ip(OTHER_SERVER))));
    assert_eq!(d.logged(Counter::AckWrongServer), 1);
    assert_eq!(d.client.phase(), Phase::Renewing);
}

#[test]
fn s_dhcp_dh_026_rebinding_takes_any_server() {
    let mut d = D::db();
    d.run(3_150_009);
    let rebind = d.timer(3_150_010);
    let xid = xid_secs(sent(&rebind)).0;
    let out = d.receive(3_200_000, &renewal_ack(xid, |o| set(o, 54, ip(OTHER_SERVER))));
    let Some(Config::Extended(lease)) = out.config else { panic!("{out:?}") };
    assert_eq!(lease.server, OTHER_SERVER);
    assert_eq!(lease.timers.unwrap().t1, at(3_150_010 + 1_800_000));
    let renew = d.timer(3_150_010 + 1_800_000);
    assert_eq!(sent(&renew).destination, OTHER_SERVER);
}

#[test]
fn s_dhcp_dh_027_a_nak_while_renewing() {
    let mut d = renewing();
    d.draws.extend([0x3333_3333, 1_000]);
    let out = d.receive(1_800_500, &nak(0x0bad_cafe, R));
    assert_eq!(out.config, Some(Config::Deconfigured(Reason::Refused)));
    assert_eq!(d.logged(Counter::Nak), 1);
    assert_eq!((message_type(sent(&out)), xid_secs(sent(&out)).0), (1, 0x3333_3333));
}

#[test]
fn s_dhcp_dh_028_a_nak_from_another_server() {
    let mut d = renewing();
    d.receive(1_800_500, &nak(0x0bad_cafe, OTHER_SERVER));
    assert_eq!(d.logged(Counter::NakWrongServer), 1);
    assert_eq!(d.client.phase(), Phase::Renewing);
}

#[test]
fn s_dhcp_dh_029_a_nak_while_requesting() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    d.draws.extend([0x4444_4444, 1_000]);
    let out = d.receive(20, &hex(V_DHCP_NAK));
    assert_eq!(out.config, None);
    assert_eq!((message_type(sent(&out)), xid_secs(sent(&out)).0), (1, 0x4444_4444));
}

#[test]
fn s_dhcp_dh_030_a_nak_loop_is_slowed() {
    let (mut d, _) = D::ds(&[XID, 1_000]);
    let mut t = 0;
    let mut delays = Vec::new();
    for _ in 0..7 {
        let xid = d.client.xid().unwrap();
        d.receive(t + 10, &server(xid, A, &offer_options()));
        let naked = t + 20;
        let immediate = d.receive(naked, &nak(xid, R));
        let restarted = match immediate.transmit {
            Some(_) => naked,
            None => {
                let at = d.deadline().unwrap();
                assert_eq!(message_type(sent(&d.timer(at))), 1);
                at
            }
        };
        delays.push(restarted - naked);
        t = restarted;
    }
    assert_eq!(delays, [0, 4_000, 8_000, 16_000, 32_000, 64_000, 64_000]);
    assert_eq!(d.count(Counter::NakBackoff), 6);
    let xid = d.client.xid().unwrap();
    d.receive(t + 10, &server(xid, A, &offer_options()));
    d.receive(t + 20, &server(xid, A, &ack_options()));
    d.verified(t + 30);
    let renew = d.timer(t + 30 + 1_800_000);
    let xid = xid_secs(sent(&renew)).0;
    assert!(d.receive(t + 30 + 1_800_100, &nak(xid, R)).transmit.is_some(), "after a BOUND the next NAK restarts at once");
}

#[test]
fn s_dhcp_dh_031_a_late_timer_rebinds_once() {
    let mut d = D::db();
    let out = d.timer(3_200_000);
    let t = sent(&out);
    assert_eq!((t.source, t.destination), (A, Ipv4Addr::BROADCAST));
    assert_eq!(d.client.phase(), Phase::Rebinding);
    assert_eq!(d.deadline(), Some(3_400_005));
    assert_eq!(d.count(Counter::TxRequest), 2, "the REQUEST that bound, and one REBIND");
}

#[test]
fn s_dhcp_dh_032_a_later_timer_expires() {
    let mut d = D::db();
    let out = d.timer(3_700_000);
    assert_eq!(out.config, Some(Config::Deconfigured(Reason::Expired)));
    assert_eq!(message_type(sent(&out)), 1);
    assert_eq!(d.count(Counter::TxRequest), 1);
}

fn times(t1: Option<u32>, t2: Option<u32>) -> (Timers, u64) {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    let mut options = ack_options();
    options.extend(t1.map(|t| (58, seconds(t))));
    options.extend(t2.map(|t| (59, seconds(t))));
    d.receive(20, &server(XID, A, &options));
    let Some(Config::Configured(lease)) = d.verified(5_020).config else { panic!() };
    (lease.timers.unwrap(), d.count(Counter::TimerOptionInvalid))
}

#[test]
fn s_dhcp_dh_033_timer_options() {
    let s = |secs: u64| at(10 + secs * 1_000);
    let check = |(t, invalid): (Timers, u64), t1: u64, t2: u64, n: u64| {
        assert_eq!((t.t1, t.t2, invalid), (s(t1), s(t2), n));
    };
    check(times(Some(600), Some(900)), 600, 900, 0);
    check(times(Some(900), Some(600)), 600, 600, 1);
    check(times(None, Some(3_600)), 1_800, 3_150, 1);
    check(times(Some(0), None), 1_800, 3_150, 1);
    check(times(Some(1_800), None), 1_800, 3_150, 0);
}

#[test]
fn s_dhcp_dh_034_an_infinite_lease() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    let mut options = ack_options();
    set(&mut options, 51, seconds(u32::MAX));
    options.push((58, seconds(600)));
    d.receive(20, &server(XID, A, &options));
    let Some(Config::Configured(lease)) = d.verified(5_020).config else { panic!() };
    assert_eq!(lease.timers, None);
    assert_eq!(d.deadline(), None);
    assert_eq!(d.count(Counter::TimerOptionInvalid), 1);
    assert_eq!(d.timer(u64::MAX / 2_000_000), toyos_dhcp::Output::default());
    assert_eq!(d.client.phase(), Phase::Bound);
}

#[test]
fn s_dhcp_dh_035_a_ten_second_lease() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    let mut options = ack_options();
    set(&mut options, 51, seconds(10));
    d.receive(20, &server(XID, A, &options));
    assert!(matches!(d.verified(4_020).config, Some(Config::Configured(_))));
    let out = d.run(10_010);
    let at: Vec<u64> = out.iter().map(|(t, _)| *t).collect();
    assert_eq!(at, [5_010, 8_760, 10_010]);
    assert_eq!(sent(&out[0].1).destination, R);
    assert_eq!(sent(&out[1].1).destination, Ipv4Addr::BROADCAST);
    assert_eq!(out[2].1.config, Some(Config::Deconfigured(Reason::Expired)));
    assert_eq!(message_type(sent(&out[2].1)), 1);
}

#[test]
fn s_dhcp_dh_036_a_lease_that_expired_while_probed() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    let mut options = ack_options();
    set(&mut options, 51, seconds(3));
    d.receive(20, &server(XID, A, &options));
    let out = d.verified(5_020);
    assert_eq!(out.config, None, "never configured");
    assert_eq!(d.logged(Counter::LeaseExpired), 1);
    assert_eq!(message_type(sent(&out)), 1);
}

#[test]
fn s_dhcp_dh_037_each_renewal_has_its_own_xid() {
    let mut d = renewing();
    d.draws.push_back(0x2222_2222);
    let second = d.timer(2_475_010);
    assert_eq!(xid_secs(sent(&second)).0, 0x2222_2222);
    d.receive(2_475_100, &renewal_ack(0x0bad_cafe, |_| {}));
    assert_eq!(d.count(Counter::XidMismatch), 1);
    let out = d.receive(2_475_200, &renewal_ack(0x2222_2222, |_| {}));
    let Some(Config::Extended(lease)) = out.config else { panic!("{out:?}") };
    assert_eq!(lease.base, at(2_475_010));
}
