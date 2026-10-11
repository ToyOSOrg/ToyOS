//! Fixtures DS and DB, a scripted random source, the byte vectors `V_*`, server messages built here and
//! checked against those vectors, and the stack a transmission is framed by.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::Ipv4Addr;

use toyos_dhcp::{AddressRequest, Client, Config, Counter, HostName, Lease, Output, Phase, Refusal, Transmission};
use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::Instant;

pub const MAC_A: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0a]);
pub const MAC_B: MacAddr = MacAddr([2, 0, 0, 0, 0, 0x0b]);
pub const MAC_R: MacAddr = MacAddr([2, 0, 0, 0, 0, 0xfe]);
pub const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const R: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 254);
pub const OTHER_SERVER: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 253);
pub const DNS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 53);
pub const DNS2: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 53);
pub const XID: u32 = 0x9a3c_21f7;
pub const CLIENT_ID: [u8; 15] = [0xff, 0, 0, 0, 0x0a, 0, 3, 0, 1, 2, 0, 0, 0, 0, 0x0a];

pub fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
}

pub fn at(ms: u64) -> Instant {
    Instant::from_millis(ms)
}

/// The DHCP payload of a frame vector.
pub fn payload_of(frame: &str) -> Vec<u8> {
    hex(frame)[42..].to_vec()
}

/// A server message: op 2, the given xid and yiaddr, chaddr A, the cookie, then `options` and END.
pub fn server(xid: u32, yiaddr: Ipv4Addr, options: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut m = vec![2, 1, 6, 0];
    m.extend_from_slice(&xid.to_be_bytes());
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&yiaddr.octets());
    m.extend_from_slice(&[0; 8]);
    m.extend_from_slice(&MAC_A.0);
    m.resize(236, 0);
    m.extend_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    for (code, data) in options {
        m.push(*code);
        m.push(data.len() as u8);
        m.extend_from_slice(data);
    }
    m.push(255);
    m
}

pub fn ip(a: Ipv4Addr) -> Vec<u8> {
    a.octets().to_vec()
}

pub fn seconds(s: u32) -> Vec<u8> {
    s.to_be_bytes().to_vec()
}

/// V-DHCP-OFFER's options, which the vectors below are checked against.
pub fn offer_options() -> Vec<(u8, Vec<u8>)> {
    vec![(53, vec![2]), (54, ip(R)), (51, seconds(3_600)), (1, vec![255, 255, 255, 0]), (3, ip(R)), (6, ip(DNS))]
}

pub fn ack_options() -> Vec<(u8, Vec<u8>)> {
    let mut o = offer_options();
    o[0] = (53, vec![5]);
    o[5] = (6, [ip(DNS), ip(DNS2)].concat());
    o.push((61, CLIENT_ID.to_vec()));
    o
}

/// An OFFER from `options` with one option replaced, added or removed.
pub fn with(options: Vec<(u8, Vec<u8>)>, code: u8, data: Option<Vec<u8>>) -> Vec<(u8, Vec<u8>)> {
    let mut o: Vec<(u8, Vec<u8>)> = options.into_iter().filter(|(c, _)| *c != code).collect();
    if let Some(data) = data {
        o.push((code, data));
    }
    o
}

pub fn offer() -> Vec<u8> {
    server(XID, A, &offer_options())
}

pub fn ack() -> Vec<u8> {
    server(XID, A, &ack_options())
}

pub fn ack_with(xid: u32, options: Vec<(u8, Vec<u8>)>) -> Vec<u8> {
    server(xid, A, &options)
}

pub fn nak(xid: u32, server_id: Ipv4Addr) -> Vec<u8> {
    server(xid, Ipv4Addr::UNSPECIFIED, &[(53, vec![6]), (54, ip(server_id))])
}

pub struct D {
    pub client: Client,
    pub draws: VecDeque<u32>,
    pub now: u64,
    pub refusals: Vec<Refusal>,
}

impl D {
    /// Fixture DS: MAC A, host name "toyos", `start` at t = 0 with the scripted draws; every
    /// draw past the script is 1,000, a zero jitter.
    pub fn ds(draws: &[u32]) -> (Self, Output) {
        Self::named(draws, HostName::new("toyos"))
    }

    pub fn named(draws: &[u32], host: Option<HostName>) -> (Self, Output) {
        let mut draws: VecDeque<u32> = draws.iter().copied().collect();
        let (client, out) = Client::start(at(0), IndividualMac::new(MAC_A).unwrap(), host, || draws.pop_front().unwrap_or(1_000));
        (Self { client, draws, now: 0, refusals: Vec::new() }, out)
    }

    /// Fixture DB: bound to 192.0.2.1/24 through 192.0.2.254, base 10, verified at 5,020.
    pub fn db() -> Self {
        let (mut d, _) = Self::ds(&[XID, 1_000, 1_000]);
        d.receive(10, &offer());
        d.receive(20, &ack());
        let out = d.verified(5_020);
        assert!(matches!(out.config, Some(Config::Configured(_))));
        assert_eq!(d.client.phase(), Phase::Bound);
        d.client.drain_refusals().for_each(drop);
        d
    }

    pub fn receive(&mut self, t: u64, payload: &[u8]) -> Output {
        self.now = t;
        let mut draws = std::mem::take(&mut self.draws);
        let out = self.client.receive(at(t), payload, R, || draws.pop_front().unwrap_or(1_000));
        self.draws = draws;
        self.collect();
        out
    }

    pub fn timer(&mut self, t: u64) -> Output {
        self.now = t;
        let mut draws = std::mem::take(&mut self.draws);
        let out = self.client.timer(at(t), || draws.pop_front().unwrap_or(1_000));
        self.draws = draws;
        self.collect();
        out
    }

    pub fn verified(&mut self, t: u64) -> Output {
        self.now = t;
        let mut draws = std::mem::take(&mut self.draws);
        let out = self.client.verified(at(t), || draws.pop_front().unwrap_or(1_000));
        self.draws = draws;
        self.collect();
        out
    }

    pub fn conflict(&mut self, t: u64, mac: MacAddr) -> Output {
        self.now = t;
        let mut draws = std::mem::take(&mut self.draws);
        let out = self.client.conflict(at(t), mac, || draws.pop_front().unwrap_or(1_000));
        self.draws = draws;
        self.collect();
        out
    }

    pub fn not_verified(&mut self, t: u64) -> Output {
        self.now = t;
        let mut draws = std::mem::take(&mut self.draws);
        let out = self.client.not_verified(at(t), || draws.pop_front().unwrap_or(1_000));
        self.draws = draws;
        self.collect();
        out
    }

    pub fn link_up(&mut self, t: u64) -> Output {
        self.now = t;
        let mut draws = std::mem::take(&mut self.draws);
        let out = self.client.link_up(at(t), || draws.pop_front().unwrap_or(1_000));
        self.draws = draws;
        self.collect();
        out
    }

    /// Fires every deadline up to `t` at its own time; every transmission, with its time.
    pub fn run(&mut self, t: u64) -> Vec<(u64, Output)> {
        let mut out = Vec::new();
        while let Some(d) = self.client.next_deadline().filter(|d| *d <= at(t)) {
            let ms = d.nanos() / 1_000_000;
            out.push((ms, self.timer(ms)));
        }
        self.now = t;
        out
    }

    fn collect(&mut self) {
        self.refusals.extend(self.client.drain_refusals());
    }

    pub fn count(&self, counter: Counter) -> u64 {
        self.client.counters().get(counter)
    }

    pub fn logged(&self, rule: Counter) -> usize {
        self.refusals.iter().filter(|r| r.rule == rule).count()
    }

    pub fn deadline(&self) -> Option<u64> {
        self.client.next_deadline().map(|d| d.nanos() / 1_000_000)
    }

    pub fn lease(&self) -> &Lease {
        self.client.lease().expect("a lease is in use")
    }
}

/// The xid and secs a transmission carries.
pub fn xid_secs(t: &Transmission) -> (u32, u16) {
    let p = &t.payload;
    (u32::from_be_bytes([p[4], p[5], p[6], p[7]]), u16::from_be_bytes([p[8], p[9]]))
}

pub fn sent(out: &Output) -> &Transmission {
    out.transmit.as_ref().expect("a transmission")
}

pub fn message_type(t: &Transmission) -> u8 {
    assert_eq!(t.payload[240..243], [53, 1, t.payload[242]]);
    t.payload[242]
}

pub fn probe_of(out: &Output) -> Option<Ipv4Addr> {
    match out.address {
        Some(AddressRequest::Probe { address, .. }) => Some(address),
        _ => None,
    }
}

pub fn ms(i: Instant) -> u64 {
    i.nanos() / 1_000_000
}

/// Frames `t` as the stack sends it: `toyos-net-ip` with 192.0.2.1/24 assigned and the router
/// resolved, the acquisition socket of `toyos-net-udp` on port 68.
pub fn framed(t: &Transmission) -> Vec<u8> {
    use toyos_net_ip::{Ip, Sent, FRAME};
    let mut ip = Ip::new(Instant::from_nanos(0), [0x5a; 16]);
    let if0 = ip.add_interface(Instant::from_nanos(0), IndividualMac::new(MAC_A).unwrap());
    ip.link_up(Instant::from_nanos(0), if0).unwrap();
    ip.add_address(Instant::from_nanos(0), if0, A, 24).unwrap();
    let setup = |ip: &mut Ip, until: Instant| {
        while let Some(d) = ip.next_deadline().filter(|d| *d <= until) {
            ip.fire(d);
            ip.transmit(d, usize::MAX, |_, _| {});
        }
    };
    setup(&mut ip, at(3_000));
    let now = at(3_000);
    let _ = ip.resolve(now, if0, R, A);
    ip.transmit(now, usize::MAX, |_, _| {});
    let mut reply = MAC_A.0.to_vec();
    reply.extend_from_slice(&MAC_R.0);
    reply.extend_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, 2]);
    reply.extend_from_slice(&MAC_R.0);
    reply.extend_from_slice(&R.octets());
    reply.extend_from_slice(&MAC_A.0);
    reply.extend_from_slice(&A.octets());
    let _ = ip.receive(now, if0, &reply);
    ip.transmit(now, usize::MAX, |_, _| {});
    let mut udp = toyos_net_udp::Udp::new();
    let socket = udp.bind(&ip, Ipv4Addr::UNSPECIFIED, toyos_net_wire::Port::new(68), || 0).unwrap();
    udp.set_acquisition(socket).unwrap();
    udp.set_broadcast(socket, true).unwrap();
    udp.send_from(&mut ip, socket, t.source, t.destination, 67, &t.payload).unwrap();
    let mut frame = Vec::new();
    let sent = udp.serve(toyos_net_udp::Sender::Socket(socket), |out| {
        let mut buf = [0u8; FRAME];
        match ip.send_udp(now, out, &mut buf) {
            Ok(Sent::Frame(n)) => frame = buf[..n].to_vec(),
            other => panic!("{other:?}"),
        }
        toyos_net_udp::Offer::Taken
    });
    assert_eq!(sent, toyos_net_udp::Served::Last, "the one datagram queued");
    frame
}

pub const V_DHCP_DISCOVER: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 39 a6 00 00 00 00 ff ff
ff ff 00 44 00 43 01 34 13 19 01 01 06 00 9a 3c
21 f7 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 01 3d 0f ff
00 00 00 0a 00 03 00 01 02 00 00 00 00 0a 39 02
05 c0 37 06 01 03 06 33 3a 3b 0c 05 74 6f 79 6f
73 50 00 ff 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00";

pub const V_DHCP_REQUEST: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 39 a6 00 00 00 00 ff ff
ff ff 00 44 00 43 01 34 08 7c 01 01 06 00 9a 3c
21 f7 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 03 36 04 c0
00 02 fe 32 04 c0 00 02 01 3d 0f ff 00 00 00 0a
00 03 00 01 02 00 00 00 00 0a 39 02 05 c0 37 06
01 03 06 33 3a 3b 0c 05 74 6f 79 6f 73 ff 00 00
00 00 00 00 00 00";

pub const V_DHCP_RENEW: &str = "
02 00 00 00 00 fe 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 b4 a5 c0 00 02 01 c0 00
02 fe 00 44 00 43 01 34 af ee 01 01 06 00 0b ad
ca fe 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 03 3d 0f ff
00 00 00 0a 00 03 00 01 02 00 00 00 00 0a 39 02
05 c0 37 06 01 03 06 33 3a 3b 0c 05 74 6f 79 6f
73 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00";

pub const V_DHCP_REBIND: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 77 a4 c0 00 02 01 ff ff
ff ff 00 44 00 43 01 34 2e a4 01 01 06 00 1c 0f
fe e5 00 00 00 00 c0 00 02 01 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 03 3d 0f ff
00 00 00 0a 00 03 00 01 02 00 00 00 00 0a 39 02
05 c0 37 06 01 03 06 33 3a 3b 0c 05 74 6f 79 6f
73 ff 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00";

pub const V_DHCP_REBOOT: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 39 a6 00 00 00 00 ff ff
ff ff 00 44 00 43 01 34 4a 66 01 01 06 00 5e ed
1e 55 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 03 32 04 c0
00 02 01 3d 0f ff 00 00 00 0a 00 03 00 01 02 00
00 00 00 0a 39 02 05 c0 37 06 01 03 06 33 3a 3b
0c 05 74 6f 79 6f 73 ff 00 00 00 00 00 00 00 00
00 00 00 00 00 00";

pub const V_DHCP_DECLINE: &str = "
ff ff ff ff ff ff 02 00 00 00 00 0a 08 00 45 00
01 48 00 00 40 00 40 11 39 a6 00 00 00 00 ff ff
ff ff 00 44 00 43 01 34 c9 ff 01 01 06 00 0d ec
11 e0 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 04 36 04 c0
00 02 fe 32 04 c0 00 02 01 3d 0f ff 00 00 00 0a
00 03 00 01 02 00 00 00 00 0a ff 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00";

pub const V_DHCP_OFFER: &str = "
02 01 06 00 9a 3c 21 f7 00 00 00 00 00 00 00 00
c0 00 02 01 00 00 00 00 00 00 00 00 02 00 00 00
00 0a 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 63 82 53 63
35 01 02 36 04 c0 00 02 fe 33 04 00 00 0e 10 01
04 ff ff ff 00 03 04 c0 00 02 fe 06 04 c0 00 02
35 ff";

pub const V_DHCP_OFFER_FRAME: &str = "
02 00 00 00 00 0a 02 00 00 00 00 fe 08 00 45 00
01 2e 00 00 40 00 40 11 b4 bf c0 00 02 fe c0 00
02 01 00 43 00 44 01 1a a8 44 02 01 06 00 9a 3c
21 f7 00 00 00 00 00 00 00 00 c0 00 02 01 00 00
00 00 00 00 00 00 02 00 00 00 00 0a 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 63 82 53 63 35 01 02 36 04 c0
00 02 fe 33 04 00 00 0e 10 01 04 ff ff ff 00 03
04 c0 00 02 fe 06 04 c0 00 02 35 ff";

pub const V_DHCP_ACK: &str = "
02 01 06 00 9a 3c 21 f7 00 00 00 00 00 00 00 00
c0 00 02 01 00 00 00 00 00 00 00 00 02 00 00 00
00 0a 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 63 82 53 63
35 01 05 36 04 c0 00 02 fe 33 04 00 00 0e 10 01
04 ff ff ff 00 03 04 c0 00 02 fe 06 08 c0 00 02
35 c6 33 64 35 3d 0f ff 00 00 00 0a 00 03 00 01
02 00 00 00 00 0a ff";

pub const V_DHCP_ACK_RAPID: &str = "
02 01 06 00 9a 3c 21 f7 00 00 00 00 00 00 00 00
c0 00 02 01 00 00 00 00 00 00 00 00 02 00 00 00
00 0a 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 63 82 53 63
35 01 05 36 04 c0 00 02 fe 33 04 00 00 0e 10 01
04 ff ff ff 00 03 04 c0 00 02 fe 06 04 c0 00 02
35 50 00 ff";

pub const V_DHCP_NAK: &str = "
02 01 06 00 9a 3c 21 f7 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 02 00 00 00
00 0a 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 63 82 53 63
35 01 06 36 04 c0 00 02 fe ff";

pub const V_DHCP_OFFER_OVERLOAD: &str = "
02 01 06 00 9a 3c 21 f7 00 00 00 00 00 00 00 00
c0 00 02 01 00 00 00 00 00 00 00 00 02 00 00 00
00 0a 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 36 04 c0 00
02 fe 33 04 00 00 0e 10 01 04 ff ff ff 00 ff 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 63 82 53 63
35 01 02 34 01 01 ff";

pub const V_DHCP_OFFER_SPLIT: &str = "
02 01 06 00 9a 3c 21 f7 00 00 00 00 00 00 00 00
c0 00 02 01 00 00 00 00 00 00 00 00 02 00 00 00
00 0a 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 63 82 53 63
35 01 02 36 04 c0 00 02 fe 33 04 00 00 0e 10 01
04 ff ff ff 00 06 03 c0 00 02 06 05 35 c6 33 64
35 ff";
