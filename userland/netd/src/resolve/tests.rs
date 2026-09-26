//! The resolver on a wire: smoltcp's own `Interface` on an Ethernet device
//! whose far end is played here frame by frame, so what is judged is what
//! leaves the interface, not what the resolver queued.

use super::*;
use std::collections::VecDeque;

use smoltcp::iface::{Config, Interface, PollResult};
use smoltcp::phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr,
    HardwareAddress, IpCidr, IpProtocol, Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr,
};

const OUR_MAC: EthernetAddress = EthernetAddress([0x02, 0, 0, 0, 0, 0x01]);
const OURS: Ipv4Address = Ipv4Address::new(10, 0, 0, 2);
/// On the link, and nothing there answers ARP: a LAN's resolver that is down.
const SILENT: [u8; 4] = [10, 0, 0, 53];
/// On the link, answering ARP and every query it is given an answer for.
const ANSWERS: [u8; 4] = [10, 0, 0, 54];
const ANSWERS_MAC: EthernetAddress = EthernetAddress([0x02, 0, 0, 0, 0, 0x54]);
/// Off the link, and the interface holds no route.
const UNROUTED: [u8; 4] = [192, 0, 2, 53];
const ADDRESS: [u8; 4] = [192, 0, 2, 7];

/// How often the loop passes here. netd's own passes are its wakes; this is
/// finer than any wait the resolver asks for.
const PASS_MS: u64 = 50;

/// Frames for the interface, and the frames it sent.
#[derive(Default)]
struct Wire {
    inbound: VecDeque<Vec<u8>>,
    outbound: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl phy::RxToken for Rx {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

impl phy::TxToken for Tx<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = vec![0u8; len];
        let result = f(&mut frame);
        self.0.push(frame);
        result
    }
}

impl Device for Wire {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _: SmolInstant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.inbound.pop_front()?;
        Some((Rx(frame), Tx(&mut self.outbound)))
    }

    fn transmit(&mut self, _: SmolInstant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.outbound))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.medium = Medium::Ethernet;
        caps
    }
}

/// What [`ANSWERS`] says for a name.
enum Says {
    Address([u8; 4]),
    /// A CNAME to this name and nothing else, which the resolver asks again
    /// at its end.
    Alias(&'static str),
}

type Draw = Box<dyn FnMut() -> u16>;

/// A draw an off-path sender could predict, which is all a test needs.
fn counter() -> Draw {
    let mut x: u32 = 0x9e37_79b9;
    Box::new(move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as u16
    })
}

struct Net {
    iface: Interface,
    wire: Wire,
    sockets: SocketSet<'static>,
    resolver: Resolver<u32, Draw>,
    born: Instant,
    now_ms: u64,
    /// [`ANSWERS`]'s zone: a name, the time before which its answer is held
    /// back, and what it says.
    zone: Vec<(&'static str, u64, Says)>,
    /// Frames [`ANSWERS`] has sent and the wire has not yet delivered, with
    /// the time they arrive.
    held: Vec<(u64, Vec<u8>)>,
    /// Every address the interface asked ARP for.
    arp_asked: Vec<[u8; 4]>,
    /// Every server a query reached.
    queried: Vec<[u8; 4]>,
    ended: Vec<(u32, Name, Result<Vec<[u8; 4]>, Ended>)>,
}

impl Net {
    fn new(servers: &[[u8; 4]]) -> Self {
        let mut wire = Wire::default();
        let mut iface =
            Interface::new(Config::new(HardwareAddress::Ethernet(OUR_MAC)), &mut wire, SmolInstant::from_millis(0));
        iface.update_ip_addrs(|addrs| addrs.push(IpCidr::new(IpAddress::Ipv4(OURS), 24)).unwrap());
        let born = Instant::now();
        let mut resolver = Resolver::new(born, counter());
        let servers: Vec<Ipv4Address> = servers.iter().map(|s| Ipv4Address::from(*s)).collect();
        resolver.set_servers(&servers);
        Self {
            iface,
            wire,
            sockets: SocketSet::new(Vec::new()),
            resolver,
            born,
            now_ms: 0,
            zone: Vec::new(),
            held: Vec::new(),
            arp_asked: Vec::new(),
            queried: Vec::new(),
            ended: Vec::new(),
        }
    }

    fn now(&self) -> Instant {
        self.born + Duration::from_millis(self.now_ms)
    }

    fn start(&mut self, client: u32, name: &str) -> Result<(), Refused> {
        let now = self.now();
        self.resolver.start(client, Name::parse(name).unwrap(), &mut self.sockets, now).map_err(|(_, why)| why)
    }

    fn poll(&mut self) {
        let at = SmolInstant::from_millis(self.now_ms as i64);
        while self.iface.poll(at, &mut self.wire, &mut self.sockets) != PollResult::None {}
    }

    /// One pass of netd's loop, then the far end's turn.
    fn pass(&mut self) {
        let now_ms = self.now_ms;
        let (due, later): (Vec<_>, Vec<_>) = self.held.drain(..).partition(|(at, _)| *at <= now_ms);
        self.held = later;
        self.wire.inbound.extend(due.into_iter().map(|(_, frame)| frame));
        self.poll();
        let now = self.now();
        let ended = self.resolver.pass(&mut self.sockets, now);
        self.ended.extend(ended);
        self.poll();
        for frame in std::mem::take(&mut self.wire.outbound) {
            self.far_end(&frame);
        }
    }

    /// Pass every [`PASS_MS`] up to and including `ms`.
    fn until(&mut self, ms: u64) {
        while self.now_ms < ms {
            self.pass();
            self.now_ms += PASS_MS;
        }
        self.now_ms = ms;
        self.pass();
    }

    /// Pass every [`PASS_MS`] until `client`'s lookup has ended or `until_ms`
    /// has come.
    fn run(&mut self, client: u32, until_ms: u64) -> Option<Result<Vec<[u8; 4]>, Ended>> {
        while self.now_ms <= until_ms {
            self.pass();
            if let Some(at) = self.ended.iter().position(|(c, ..)| *c == client) {
                return Some(self.ended.remove(at).2);
            }
            self.now_ms += PASS_MS;
        }
        None
    }

    fn far_end(&mut self, frame: &[u8]) {
        let frame = EthernetFrame::new_checked(frame).expect("the interface sent an Ethernet frame");
        match frame.ethertype() {
            EthernetProtocol::Arp => {
                let arp = ArpRepr::parse(&ArpPacket::new_checked(frame.payload()).unwrap()).unwrap();
                let ArpRepr::EthernetIpv4 { operation: ArpOperation::Request, target_protocol_addr, .. } = arp else {
                    return;
                };
                self.arp_asked.push(target_protocol_addr.octets());
                if target_protocol_addr.octets() == ANSWERS {
                    let reply = ArpRepr::EthernetIpv4 {
                        operation: ArpOperation::Reply,
                        source_hardware_addr: ANSWERS_MAC,
                        source_protocol_addr: Ipv4Address::from(ANSWERS),
                        target_hardware_addr: OUR_MAC,
                        target_protocol_addr: OURS,
                    };
                    let mut out = vec![0u8; 14 + reply.buffer_len()];
                    let mut eth = EthernetFrame::new_unchecked(&mut out);
                    EthernetRepr { src_addr: ANSWERS_MAC, dst_addr: OUR_MAC, ethertype: EthernetProtocol::Arp }
                        .emit(&mut eth);
                    reply.emit(&mut ArpPacket::new_unchecked(eth.payload_mut()));
                    self.wire.inbound.push_back(out);
                }
            }
            EthernetProtocol::Ipv4 => {
                let ip = Ipv4Packet::new_checked(frame.payload()).unwrap();
                assert_eq!(ip.next_header(), IpProtocol::Udp, "the resolver sends only UDP");
                let udp = UdpPacket::new_checked(ip.payload()).unwrap();
                assert_eq!(udp.dst_port(), toyos_dns::PORT);
                let to = ip.dst_addr().octets();
                self.queried.push(to);
                assert_eq!(to, ANSWERS, "a query left for a server the wire cannot reach");
                self.answer(udp.src_port(), udp.payload());
            }
            other => panic!("the interface sent a frame of type {other}"),
        }
    }

    /// [`ANSWERS`]'s reply to `query`, from its port 53 to `port`, spelled by
    /// hand: the question echoed and one answer record behind a pointer to it.
    fn answer(&mut self, port: u16, query: &[u8]) {
        let asked = question(query);
        let Some((_, not_before, says)) = self.zone.iter().find(|(name, ..)| *name == asked) else {
            return;
        };
        let mut reply = query.to_vec();
        reply[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
        reply[6..8].copy_from_slice(&1u16.to_be_bytes());
        reply.extend_from_slice(&[0xc0, 12]);
        let (rtype, data) = match says {
            Says::Address(addr) => (1u16, addr.to_vec()),
            Says::Alias(target) => (5, Name::parse(target).unwrap().wire().to_vec()),
        };
        reply.extend_from_slice(&rtype.to_be_bytes());
        reply.extend_from_slice(&1u16.to_be_bytes());
        reply.extend_from_slice(&60u32.to_be_bytes());
        reply.extend_from_slice(&(data.len() as u16).to_be_bytes());
        reply.extend_from_slice(&data);

        let caps = ChecksumCapabilities::default();
        let udp = UdpRepr { src_port: toyos_dns::PORT, dst_port: port };
        let ip = Ipv4Repr {
            src_addr: Ipv4Address::from(ANSWERS),
            dst_addr: OURS,
            next_header: IpProtocol::Udp,
            payload_len: udp.header_len() + reply.len(),
            hop_limit: 64,
        };
        let mut out = vec![0u8; 14 + ip.buffer_len() + ip.payload_len];
        let mut eth = EthernetFrame::new_unchecked(&mut out);
        EthernetRepr { src_addr: ANSWERS_MAC, dst_addr: OUR_MAC, ethertype: EthernetProtocol::Ipv4 }.emit(&mut eth);
        let mut packet = Ipv4Packet::new_unchecked(eth.payload_mut());
        ip.emit(&mut packet, &caps);
        udp.emit(
            &mut UdpPacket::new_unchecked(packet.payload_mut()),
            &IpAddress::Ipv4(ip.src_addr),
            &IpAddress::Ipv4(ip.dst_addr),
            reply.len(),
            |payload| payload.copy_from_slice(&reply),
            &caps,
        );
        self.held.push(((*not_before).max(self.now_ms), out));
    }
}

/// The dotted name a query asks, read by hand.
fn question(query: &[u8]) -> String {
    let mut labels = Vec::new();
    let mut at = 12;
    while query[at] != 0 {
        let len = usize::from(query[at]);
        labels.push(std::str::from_utf8(&query[at + 1..at + 1 + len]).unwrap().to_string());
        at += 1 + len;
    }
    labels.join(".")
}

/// **A query to a server whose link address never resolves holds up no other
/// query.** smoltcp keeps a datagram at the head of its socket's queue while
/// its neighbour is missing, so a query queued behind it on the same socket
/// never leaves. The first server is on the link and answers no ARP; the
/// second is asked when the first query's wait is over, and answers.
#[test]
fn a_server_that_answers_no_arp_holds_up_no_other_server() {
    let mut net = Net::new(&[SILENT, ANSWERS]);
    net.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    net.start(1, "www.example").unwrap();
    let ended = net.run(1, 25_000);
    assert!(net.arp_asked.contains(&SILENT), "the premise: the first server was asked for its link address");
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the second server was never asked: it heard {:?}", net.queried);
    assert!(net.now_ms < 2 * toyos_dns::WAIT_MS, "answered at {} ms", net.now_ms);
}

/// The same, for a server no route leads to: smoltcp keeps that datagram at
/// the head of its queue too.
#[test]
fn a_server_with_no_route_holds_up_no_other_server() {
    let mut net = Net::new(&[UNROUTED, ANSWERS]);
    net.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    net.start(1, "www.example").unwrap();
    let ended = net.run(1, 25_000);
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the second server was never asked: it heard {:?}", net.queried);
    assert!(net.now_ms < 2 * toyos_dns::WAIT_MS, "answered at {} ms", net.now_ms);
}

/// **An alias answered late, while queries sit behind a missing neighbour,
/// restarts the lookup without overflowing anything.** The first server
/// answers its first query only after five more have been sent, the second
/// server's among them, and answers with an alias alone, so the lookup asks
/// again at the alias with every query afresh. The reply reaches netd from
/// the network, so nothing it arranges may end netd.
#[test]
fn an_alias_answered_while_queries_are_stuck_restarts_the_lookup() {
    let mut net = Net::new(&[ANSWERS, SILENT]);
    net.zone.push(("www.example", 10_500, Says::Alias("cdn.example")));
    net.zone.push(("cdn.example", 0, Says::Address(ADDRESS)));
    net.start(1, "www.example").unwrap();
    let ended = net.run(1, 40_000);
    assert_eq!(ended, Some(Ok(vec![ADDRESS])), "the servers heard {:?}", net.queried);
}

/// **At most [`MAX_LOOKUPS`] are in flight**, and one past it is refused with
/// the code a client reads as "this machine is full".
#[test]
fn the_lookup_past_the_cap_is_refused_as_exhausted() {
    let mut net = Net::new(&[SILENT]);
    for client in 0..MAX_LOOKUPS as u32 {
        net.start(client, "www.example").unwrap_or_else(|_| panic!("lookup {client} was refused"));
    }
    let refused = net.start(MAX_LOOKUPS as u32, "www.example").expect_err("one lookup past the cap");
    assert!(matches!(refused, Refused::Full));
    assert_eq!(refused.code(), toyos::net::ERR_RESOURCE_EXHAUSTED);
}

/// **A lookup whose client has left is let go at once**: every socket its
/// queries left from leaves the stack, and its slot takes another lookup.
#[test]
fn a_lookup_whose_client_left_is_let_go_at_once() {
    let mut net = Net::new(&[ANSWERS]);
    for client in 0..MAX_LOOKUPS as u32 {
        net.start(client, "www.example").unwrap();
    }
    net.until(toyos_dns::WAIT_MS);
    assert!(net.ended.is_empty(), "nothing answers, so nothing has ended");
    assert_eq!(net.queried.len(), 2 * MAX_LOOKUPS, "two queries each left, neither answered");
    assert_eq!(net.resolver.sockets(), 2 * MAX_LOOKUPS);
    assert_eq!(net.sockets.iter().count(), 2 * MAX_LOOKUPS);
    net.resolver.let_go(&mut net.sockets, |&client| client == 3);
    assert_eq!(net.sockets.iter().count(), 2 * (MAX_LOOKUPS - 1), "both of its sockets left the stack");
    assert_eq!(net.resolver.sockets(), 2 * (MAX_LOOKUPS - 1));
    assert!(net.resolver.clients().all(|&client| client != 3));
    net.start(MAX_LOOKUPS as u32, "www.example").expect("the slot it left takes another lookup");
}

/// **Each query waiting for its answer holds one socket; one whose wait ended
/// before it left holds none, and neither does a lookup that ended.**
#[test]
fn a_lookup_holds_a_socket_per_query_that_left_and_none_once_ended() {
    let mut net = Net::new(&[ANSWERS, SILENT]);
    net.start(1, "www.example").unwrap();
    net.until(toyos_dns::WAIT_MS - PASS_MS);
    assert_eq!(net.queried, [ANSWERS]);
    assert_eq!(net.sockets.iter().count(), 1);
    net.until(toyos_dns::WAIT_MS);
    assert_eq!(net.sockets.iter().count(), 2, "the first query is still answered, the second is new");
    net.until(2 * toyos_dns::WAIT_MS);
    assert_eq!(net.queried, [ANSWERS, ANSWERS], "the second query never left");
    assert_eq!(net.sockets.iter().count(), 2, "the second query's socket went when its wait ended");
    net.zone.push(("www.example", 0, Says::Address(ADDRESS)));
    assert_eq!(net.run(1, 5 * toyos_dns::WAIT_MS), Some(Ok(vec![ADDRESS])), "the fifth query, to the first server");
    assert_eq!(net.sockets.iter().count(), 0, "an ended lookup holds no socket");
}
