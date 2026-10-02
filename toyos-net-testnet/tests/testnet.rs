//! The test network's own promises, which no scenario names: its capture is the classic pcap of
//! exactly what the devices carried, and a run repeats exactly.

use std::net::Ipv4Addr;
use std::time::Duration;

use etherparse::{LinkSlice, NetSlice, SlicedPacket, TransportSlice};
use pcap_file::pcap::PcapReader;
use pcap_file::{DataLink, Endianness, TsResolution};
use toyos_net_tcp::Endpoint;
use toyos_net_testnet::{Fate, Net};
use toyos_net_wire::ethernet::MacAddr;
use toyos_net_wire::{Instant, Port};

const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
const MAC_A: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0a]);
const MAC_B: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0b]);

/// A writes 64 KiB to B and B 16 KiB back, every 7th frame from A lost.
fn transfer() -> Net {
    let mut net = Net::new(Instant::from_millis(3_600_000));
    let a = net.add_node(MAC_A, A, 24);
    let b = net.add_node(MAC_B, B, 24);
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

/// Architecture §5.3 (f): a published pcap reader and a published Ethernet/IPv4/TCP parser,
/// neither written here, read back each frame and instant the devices carried.
#[test]
fn a_published_reader_decodes_the_capture_as_the_wire_carried_it() {
    let net = transfer();
    let file = net.pcap();
    let mut reader = PcapReader::new(&file[..]).expect("a pcap file");
    let header = reader.header();
    assert_eq!((header.version_major, header.version_minor, header.ts_correction, header.ts_accuracy, header.snaplen), (2, 4, 0, 0, 65_535));
    assert_eq!((header.datalink, header.ts_resolution, header.endianness), (DataLink::ETHERNET, TsResolution::MicroSecond, Endianness::Little));
    let nodes = [(MAC_A, A), (MAC_B, B)];
    let (mut tcp, mut arp) = (0, 0);
    for carried in net.wire() {
        let record = reader.next_packet().expect("a record for every frame").expect("a well-formed record");
        assert_eq!(record.timestamp, Duration::from_micros(carried.at.nanos() / 1_000));
        assert_eq!(record.orig_len as usize, carried.frame.len(), "the whole frame is captured");
        assert_eq!(record.data[..], carried.frame[..]);
        let (mac, addr) = nodes[carried.from];
        let packet = SlicedPacket::from_ethernet(&record.data).expect("Ethernet II");
        let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
        assert_eq!(ethernet.source(), mac.0);
        match (&packet.net, &packet.transport) {
            (Some(NetSlice::Ipv4(ip)), Some(TransportSlice::Tcp(segment))) => {
                let ip = ip.header();
                assert_eq!(ip.source(), addr.octets());
                assert_eq!(ip.header_checksum(), ip.to_header().calc_header_checksum(), "the IPv4 header checksum");
                assert!(ip.dont_fragment());
                let sum = segment.calc_checksum_ipv4(ip.source(), ip.destination()).expect("a segment within IPv4's length");
                assert_eq!(segment.checksum(), sum, "the TCP checksum");
                tcp += 1;
            }
            (Some(NetSlice::Arp(packet)), None) => {
                assert_eq!(packet.sender_hw_addr(), mac.0);
                // A probe's sender address is 0.0.0.0 (RFC 5227 §2.1.1).
                assert!([addr.octets(), [0; 4]].iter().any(|ours| packet.sender_protocol_addr() == ours), "{packet:?}");
                arp += 1;
            }
            other => panic!("neither TCP in IPv4 nor ARP: {other:?}"),
        }
    }
    assert!(reader.next_packet().is_none(), "no record beyond the wire");
    assert!(tcp > 100 && arp > 0, "{tcp} TCP segments, {arp} ARP packets");
}
