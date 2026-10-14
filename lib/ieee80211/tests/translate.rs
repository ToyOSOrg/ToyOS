//! MSDUs out of data frames, and Ethernet frames into them and back.

mod common;

use common::{qos_from_ds, subframe, AP, PEER, STA};
use toyos_ieee80211::data::{llc_snap, msdus, Ethernet, Msdu, ToDs};
use toyos_ieee80211::frame::{Ds, Frame};
use toyos_ieee80211::{Address, Seq};

#[test]
fn an_amsdu_yields_each_subframe_with_its_own_addresses() {
    let other = Address([0x02, 9, 9, 9, 9, 9]);
    let body = [subframe(STA, PEER, b"abc", false), subframe(STA, other, b"defgh", true)].concat();
    let f = qos_from_ds(true, &body);
    let Ok(Frame::Data(d)) = Frame::parse(&f) else { panic!("not data") };
    let got: Vec<Msdu> = msdus(&d, d.body).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        got,
        [Msdu { da: STA, sa: PEER, payload: b"abc" }, Msdu { da: STA, sa: other, payload: b"defgh" }]
    );
}

#[test]
fn a_frame_without_the_amsdu_bit_is_one_msdu_between_its_header_addresses() {
    let f = qos_from_ds(false, b"payload");
    let Ok(Frame::Data(d)) = Frame::parse(&f) else { panic!("not data") };
    let got: Vec<Msdu> = msdus(&d, d.body).unwrap().map(Result::unwrap).collect();
    // From DS: Address 1 is the destination, Address 3 the source.
    assert_eq!(got, [Msdu { da: STA, sa: PEER, payload: b"payload" }]);
}

#[test]
fn a_null_frame_carries_no_msdu() {
    let mut f = qos_from_ds(false, &[]);
    f[0] = 0xC8;
    let Ok(Frame::Data(d)) = Frame::parse(&f) else { panic!("not data") };
    assert!(d.null);
    assert_eq!(msdus(&d, d.body).unwrap().count(), 0);
}

#[test]
fn an_ethernet_frame_sent_to_the_ap_reads_back_as_itself() {
    let header = ToDs { bssid: AP, sa: STA, da: PEER, seq: Seq::new(4095), tid: 6, protected: false };
    let payload = b"\x45\x00 an ipv4 datagram";
    let f = [&header.header()[..], &llc_snap(0x0800).unwrap(), payload].concat();
    let Ok(Frame::Data(d)) = Frame::parse(&f) else { panic!("not data") };
    assert_eq!((d.ds, d.ra(), d.seq, d.qos.unwrap().tid()), (Ds::ToDs, AP, Seq::new(4095), 6));
    let msdu = msdus(&d, d.body).unwrap().next().unwrap().unwrap();
    assert_eq!(
        Ethernet::from_msdu(msdu).unwrap(),
        Ethernet { dst: PEER, src: STA, ethertype: 0x0800, payload }
    );
}
