//! §D8 and §D9: link changes, INIT-REBOOT and lost addresses.

mod common;

use std::net::Ipv4Addr;

use common::*;
use toyos_dhcp::{AddressRequest, Config, Counter, Phase, Reason};

fn rebooting() -> D {
    let mut d = D::db();
    d.draws.extend([0x5eed_1e55, 1_000]);
    d.link_up(100_000);
    d
}

fn reboot_ack(xid: u32, yiaddr: Ipv4Addr) -> Vec<u8> {
    server(xid, yiaddr, &with(ack_options(), 54, Some(ip(OTHER_SERVER))))
}

#[test]
fn s_dhcp_dh_064_link_up_with_a_lease() {
    let mut d = D::db();
    d.draws.extend([0x5eed_1e55, 1_000]);
    let out = d.link_up(100_000);
    let t = sent(&out);
    assert_eq!(t.payload, payload_of(V_DHCP_REBOOT));
    assert_eq!(framed(t), hex(V_DHCP_REBOOT));
    assert_eq!((out.config, out.address), (None, None), "the address stays configured; [ip] announces it");
    assert_eq!(d.client.phase(), Phase::Rebooting);
    assert_eq!(d.client.lease().map(|l| l.address), Some(A));
    assert_eq!(d.deadline(), Some(104_000));
}

#[test]
fn s_dhcp_dh_065_the_reboot_is_acknowledged() {
    let mut d = rebooting();
    let out = d.receive(100_050, &reboot_ack(0x5eed_1e55, A));
    let Some(Config::Extended(lease)) = out.config else { panic!("{out:?}") };
    assert_eq!((lease.base, lease.timers.unwrap().t1), (at(100_000), at(1_900_000)));
    assert_eq!(d.client.phase(), Phase::Bound);
}

#[test]
fn s_dhcp_dh_066_the_reboot_is_refused() {
    let mut d = rebooting();
    let out = d.receive(100_050, &nak(0x5eed_1e55, R));
    assert_eq!(out.config, Some(Config::Deconfigured(Reason::Refused)));
    assert_eq!(message_type(sent(&out)), 1);
}

#[test]
fn s_dhcp_dh_067_an_unanswered_reboot_keeps_the_lease() {
    let mut d = rebooting();
    let out = d.run(128_000);
    assert_eq!(out.iter().map(|(t, _)| *t).collect::<Vec<_>>(), [104_000, 112_000, 128_000]);
    assert!(out.iter().all(|(_, o)| message_type(sent(o)) == 3));
    let give_up = d.timer(160_000);
    assert_eq!(give_up, toyos_dhcp::Output::default());
    assert_eq!(d.count(Counter::RebootUnanswered), 1);
    assert_eq!(d.client.phase(), Phase::Bound);
    let timers = d.lease().timers.unwrap();
    assert_eq!((timers.t1, timers.expiry), (at(1_800_010), at(3_600_010)));
    assert_eq!(d.deadline(), Some(1_800_010));
}

#[test]
fn s_dhcp_dh_068_the_lease_expires_while_rebooting() {
    let mut d = D::db();
    d.link_up(3_590_000);
    let out = d.run(3_600_010);
    let (t, last) = out.last().unwrap();
    assert_eq!(*t, 3_600_010);
    assert_eq!(last.config, Some(Config::Deconfigured(Reason::Expired)));
    assert_eq!(message_type(sent(last)), 1);
}

#[test]
fn s_dhcp_dh_069_a_reboot_ack_for_another_address() {
    let mut d = rebooting();
    d.receive(100_050, &reboot_ack(0x5eed_1e55, Ipv4Addr::new(192, 0, 2, 7)));
    assert_eq!(d.logged(Counter::AckAddressChanged), 1);
    assert_eq!(d.client.phase(), Phase::Rebooting);
}

#[test]
fn s_dhcp_dh_070_link_up_while_selecting() {
    let (mut d, _) = D::ds(&[XID, 1_000, 0x5555_5555, 1_000]);
    let out = d.link_up(2_000);
    assert_eq!(xid_secs(sent(&out)), (0x5555_5555, 0));
    assert_eq!(d.deadline(), Some(6_000));
}

#[test]
fn s_dhcp_dh_071_link_up_while_probing() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000]);
    d.receive(10, &offer());
    d.receive(20, &ack());
    let out = d.link_up(30);
    assert_eq!(out.address, Some(AddressRequest::Cancel(A)));
    assert_eq!(message_type(sent(&out)), 1);
    assert_eq!(d.client.phase(), Phase::Selecting);
}

#[test]
fn s_dhcp_dh_072_an_address_lost_in_use() {
    let mut d = D::db();
    d.draws.push_back(0x0dec_11e0);
    let out = d.conflict(500_000, MAC_B);
    assert_eq!(out.config, Some(Config::Deconfigured(Reason::Conflict)));
    assert_eq!(sent(&out).payload, payload_of(V_DHCP_DECLINE));
    assert_eq!(d.client.phase(), Phase::BackingOff);
    assert_eq!(d.deadline(), Some(510_000));
    assert_eq!(message_type(sent(&d.timer(510_000))), 1);
}

#[test]
fn s_dhcp_dh_076_the_link_went_down_while_probing() {
    let (mut d, _) = D::ds(&[XID, 1_000, 1_000, 0x6666_6666, 1_000]);
    d.receive(10, &offer());
    d.receive(20, &ack());
    let out = d.not_verified(40);
    assert_eq!(out.config, None);
    assert_eq!(xid_secs(sent(&out)), (0x6666_6666, 0));
    assert_eq!(message_type(sent(&out)), 1);
}
