//! The test network's own promises, which no scenario names (QUESTIONS Q4): its capture is the
//! classic pcap of exactly what the devices carried, and a run repeats exactly.

use std::net::Ipv4Addr;
use std::time::Duration;

use toyos_net_tcp::Endpoint;
use toyos_net_testnet::{Fate, Net};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::{Instant, Port};

const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);

/// A writes 64 KiB to B and B 16 KiB back, every 7th frame from A lost.
fn transfer() -> Net {
    let mut net = Net::new(Instant::from_millis(3_600_000));
    let a = net.add_node(MacAddr([2, 0, 0, 0, 0, 0x0a]), A, 24);
    let b = net.add_node(MacAddr([2, 0, 0, 0, 0, 0x0b]), B, 24);
    net.advance(Duration::from_secs(1));
    let mut seen = 0;
    net.link(a, b).rule = Some(Box::new(move |_| {
        seen += 1;
        if seen % 7 == 0 {
            Fate::Drop
        } else {
            Fate::Pass
        }
    }));
    net.serve(b, 80, 16 * 1024);
    net.open(a, Endpoint { addr: B, port: Port::new(80).unwrap() }, 64 * 1024);
    assert!(net.run_until(Duration::from_secs(60), |net| net.finished()));
    net.assert_exact();
    net
}

#[test]
fn a_run_repeats_exactly() {
    let (first, second) = (transfer(), transfer());
    let record = |net: &Net| net.wire().iter().map(|c| (c.at, c.from, c.frame.clone())).collect::<Vec<_>>();
    assert!(first.wire().len() > 100);
    assert!(record(&first) == record(&second), "two runs put different frames on the wire");
}

#[test]
fn a_capture_is_a_classic_pcap_of_the_wire() {
    let net = transfer();
    let file = net.pcap();
    let u32_at = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
    let u16_at = |at: usize| u16::from_le_bytes(file[at..at + 2].try_into().unwrap());
    assert_eq!(file[..4], [0xd4, 0xc3, 0xb2, 0xa1], "the microsecond magic, little-endian");
    assert_eq!((u16_at(4), u16_at(6), u32_at(8), u32_at(12), u32_at(16), u32_at(20)), (2, 4, 0, 0, 65_535, 1), "version 2.4, UTC, snaplen, Ethernet");
    let mut at = 24;
    for carried in net.wire() {
        let micros = carried.at.nanos() / 1_000;
        assert_eq!((u64::from(u32_at(at)), u64::from(u32_at(at + 4))), (micros / 1_000_000, micros % 1_000_000));
        let len = carried.frame.len();
        assert_eq!((u32_at(at + 8), u32_at(at + 12)), (len as u32, len as u32), "the whole frame is captured");
        assert_eq!(file[at + 16..at + 16 + len], carried.frame[..]);
        at += 16 + len;
    }
    assert_eq!(at, file.len());
}
