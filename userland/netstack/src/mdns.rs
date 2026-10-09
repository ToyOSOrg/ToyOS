//! This machine's name on its network: `<hostname>.local` answers with the
//! address the lease gave it (`toyos_mdns`, RFC 6762), so another machine on
//! the network reaches it by name with nothing configured on either — the
//! development host finds the T14's served log this way.
//!
//! **Answered only while an address is held on a link that is up, and the
//! name is claimed**: every address after none and every return of the link
//! is probed on first (§8, §8.1), which takes the name's first answer three
//! quarters of a second and a drawn delay past it, and a name another host
//! answers for is not this machine's. Every decision is
//! `toyos_mdns::Responder`'s; this is the socket, the clock, the draw of the
//! delay the responder asks for, and the log line for what became of the
//! name. A pass tells the responder its link, hands it every message that
//! has arrived, and only then asks what the name is owed: a conflicting
//! response received as a probing ends takes the name before it is claimed.
//! What the name is owed later — a probe, an announcement (§8.3), or an
//! answer §6 held back — is a wake of netstack's own loop
//! ([`Responder::wake_in`]) rather than a sleep, because the protocol names
//! the interval and nothing on the wire says when it has passed.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpCidr, IpEndpoint};
use toyos_mdns::{Event, Host, Link, Source, To, GROUP, PORT};

/// A message is a few hundred bytes; this holds a handful of them between two
/// passes, and one past it is dropped by the socket, which is what its
/// sender's own retry is for.
const BUFFER: usize = 4096;

pub struct Responder<D> {
    handle: SocketHandle,
    host: &'static str,
    record: toyos_mdns::Responder<'static>,
    /// The origin of the responder's clock.
    born: Instant,
    /// The delay before each probing's first probe.
    draw: D,
}

impl<D: FnMut() -> u16> Responder<D> {
    /// Join the group and bind its port. `host` is the name this machine asks
    /// its network to record for it (`dhcp::HOSTNAME`), and `draw` the source
    /// of each probing's delay.
    pub fn new(host: &'static str, iface: &mut Interface, socket_set: &mut SocketSet<'static>, draw: D) -> Self {
        let label = Host::new(host).unwrap_or_else(|_| panic!("netstack: {host:?} is no host name"));
        iface
            .join_multicast_group(IpAddress::Ipv4(Ipv4Addr::from(GROUP)))
            .expect("netstack: the multicast DNS group is the one group this interface joins");
        let buffer = || {
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; BUFFER])
        };
        let mut socket = udp::Socket::new(buffer(), buffer());
        socket.bind(PORT).expect("netstack: nothing else binds the multicast DNS port");
        Self { handle: socket_set.add(socket), host, record: toyos_mdns::Responder::new(label), born: Instant::now(), draw }
    }

    /// After each poll: tell the responder its link, which is the interface's
    /// address while the card's link is up; hand it every message that
    /// arrived and send what each is answered; then send what the name is
    /// owed now. What became of the name is said as it happens.
    pub fn pass(&mut self, iface: &Interface, socket_set: &mut SocketSet<'_>, link_up: bool, now: Instant) {
        let socket = socket_set.get_mut::<udp::Socket>(self.handle);
        // IPv4 is the one protocol this netstack is built with, so every address is one.
        let link = iface.ip_addrs().first().filter(|_| link_up).map(|&IpCidr::Ipv4(cidr)| Link {
            addr: cidr.address().octets(),
            prefix: cidr.prefix_len(),
        });
        let now_ms = self.ms(now);
        let group = IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::from(GROUP)), PORT);
        self.record.on(link, now_ms);
        while let Ok((message, meta)) = socket.recv() {
            let IpAddress::Ipv4(from) = meta.endpoint.addr;
            let from = Source { addr: from.octets(), port: meta.endpoint.port };
            let (answer, event) = self.record.heard(message, from, now_ms);
            said(self.host, event);
            let Some(answer) = answer else {
                continue;
            };
            let to = match answer.to {
                To::Group => group,
                To::Asker => meta.endpoint,
            };
            send(socket, &answer.bytes, to);
        }
        let (owed, event) = self.record.owed(now_ms, || u32::from((self.draw)()));
        said(self.host, event);
        if let Some(owed) = owed {
            send(socket, &owed, group);
        }
    }

    /// When the loop must wake for what the name is owed, if anything is.
    pub fn wake_in(&self, now: Instant) -> Option<Duration> {
        self.record.owed_at().map(|at| Duration::from_millis(at.saturating_sub(self.ms(now))))
    }

    fn ms(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.born).as_millis() as u64
    }
}

/// The log line for what became of `host`'s name.
fn said(host: &str, event: Option<Event>) {
    match event {
        Some(Event::Claimed) => crate::say!("netstack: mDNS: no host answered for {host}.local; this machine answers as it"),
        Some(Event::Lost) => crate::say!("netstack: mDNS: another host answered for {host}.local; this machine answers to no name"),
        None => {}
    }
}

fn send(socket: &mut udp::Socket, bytes: &[u8], to: IpEndpoint) {
    match socket.send_slice(bytes, to) {
        Ok(()) => {}
        // A burst of queries this pass cannot answer; the asker retries, and
        // nothing here waits.
        Err(udp::SendError::BufferFull) => {}
        // Only an asker's own source can be this, an address or a port of
        // zero, and nothing on the wire reaches it.
        Err(udp::SendError::Unaddressable) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::iface::{Config, PollResult};
    use smoltcp::phy::{Device, Loopback, Medium, TxToken};
    use smoltcp::time::Instant as PollInstant;
    use smoltcp::wire::{EthernetAddress, HardwareAddress, Ipv4Address};

    /// The bytes of a probe for `t14.local` (RFC 6762 §8.1, RFC 1035 §4.1): a
    /// 12-byte header, the 11-byte name with its type and class, and the
    /// proposed record, the name again and 14 bytes behind it.
    const PROBE: usize = 12 + 11 + 4 + 11 + 14;
    /// The bytes of an announcement: the header and the record.
    const ANNOUNCEMENT: usize = 12 + 11 + 14;

    /// The responder as `main` runs it, over smoltcp's loopback device: a
    /// frame handed to the device is the next one the interface receives.
    struct Machine {
        device: Loopback,
        iface: Interface,
        sockets: SocketSet<'static>,
        mdns: Responder<fn() -> u16>,
    }

    impl Machine {
        /// 10.0.2.15/24, its delay drawn as zero.
        fn new() -> Self {
            let mut device = Loopback::new(Medium::Ethernet);
            let config = Config::new(HardwareAddress::Ethernet(EthernetAddress([2, 0, 0, 0, 0, 1])));
            let mut iface = Interface::new(config, &mut device, PollInstant::from_millis(0));
            iface.update_ip_addrs(|addrs| addrs.push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 15)), 24)).unwrap());
            let mut sockets = SocketSet::new(Vec::new());
            let mdns = Responder::new("t14", &mut iface, &mut sockets, (|| 0) as fn() -> u16);
            Self { device, iface, sockets, mdns }
        }

        /// One pass of `main`'s loop `ms` after the responder was born: the
        /// poll, then the name's pass. Returns how many bytes of messages the
        /// pass left in the responder's socket.
        fn pass(&mut self, ms: u64, link_up: bool) -> usize {
            let poll = PollInstant::from_millis(ms as i64);
            while self.iface.poll(poll, &mut self.device, &mut self.sockets) != PollResult::None {}
            let before = self.sockets.get::<udp::Socket>(self.mdns.handle).send_queue();
            self.mdns.pass(&self.iface, &mut self.sockets, link_up, self.mdns.born + Duration::from_millis(ms));
            self.sockets.get::<udp::Socket>(self.mdns.handle).send_queue() - before
        }

        fn wake_in(&self, ms: u64) -> Option<Duration> {
            self.mdns.wake_in(self.mdns.born + Duration::from_millis(ms))
        }

        /// A neighbour at 10.0.2.7 multicasts `message` from the group's
        /// port: Ethernet II to RFC 1112 §6.4's address, RFC 791's header
        /// with TTL 255 and RFC 768's, their checksums zero, which the
        /// loopback device does not ask for.
        fn hears(&mut self, message: &[u8]) {
            let udp = 8 + message.len() as u16;
            let total = 20 + udp;
            let mut frame = vec![0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb, 2, 0, 0, 0, 0, 7, 0x08, 0x00];
            frame.extend_from_slice(&[0x45, 0, (total >> 8) as u8, total as u8, 0, 0, 0, 0, 255, 17, 0, 0, 10, 0, 2, 7, 224, 0, 0, 251]);
            frame.extend_from_slice(&[0x14, 0xe9, 0x14, 0xe9, (udp >> 8) as u8, udp as u8, 0, 0]);
            frame.extend_from_slice(message);
            let token = self.device.transmit(PollInstant::from_millis(0)).expect("the loopback device always has room");
            token.consume(frame.len(), |buffer| buffer.copy_from_slice(&frame));
        }
    }

    /// **A wake is asked for exactly what the name owes.** Nothing before an
    /// address is held; the first probe's own instant, the drawn delay after
    /// the address, once a pass schedules it. A responder that never asks for
    /// this wake sends that probe, and every one after it, only on some
    /// other, unrelated wake.
    #[test]
    fn wake_in_asks_for_what_the_name_owes_and_nothing_else() {
        let mut m = Machine::new();
        assert_eq!(m.wake_in(0), None, "nothing is owed before an address is held");
        m.mdns.draw = || 100;
        assert_eq!(m.pass(0, true), 0, "§8.1: the delay before the first probe");
        assert_eq!(m.wake_in(0), Some(Duration::from_millis(100)));
        assert_eq!(m.wake_in(40), Some(Duration::from_millis(60)));
    }

    /// RFC 6762 §8: "Whenever a Multicast DNS responder ... receives an
    /// indication of a network interface "Link Change" event ... it MUST
    /// perform the two startup steps below: Probing (Section 8.1) and
    /// Announcing (Section 8.3)." The interface keeps its address across the
    /// card's link, so `main`'s word that the link is down is what tells the
    /// responder: it owes a link that is down nothing, and probes for the
    /// name from the start when the link is back.
    #[test]
    fn a_link_that_is_down_is_owed_nothing_and_its_return_is_probed_on() {
        let mut m = Machine::new();
        assert_eq!((m.pass(0, true), m.pass(250, true), m.pass(500, true)), (PROBE, PROBE, PROBE));
        assert_eq!((m.pass(750, true), m.pass(1_750, true)), (ANNOUNCEMENT, ANNOUNCEMENT), "§8.3: claimed, and announced twice");
        assert_eq!(m.wake_in(1_750), None);

        assert_eq!(m.pass(5_000, false), 0);
        assert_eq!(m.wake_in(5_000), None, "nothing is owed a link that is down");
        assert_eq!(m.pass(9_000, true), PROBE, "the link is back, and probed on");
        assert_eq!((m.pass(9_250, true), m.pass(9_500, true), m.pass(9_750, true)), (PROBE, PROBE, ANNOUNCEMENT), "and announced only then");
    }

    /// RFC 6762 §8.1: "If, by 250 ms after the third probe, no conflicting
    /// Multicast DNS responses have been received, the host may move to the
    /// next step, announcing." A response in the socket when the pass that
    /// ends the 250 ms begins has been received: it takes the name, and
    /// nothing is announced. The message is RFC 1035 §4.1's: a response
    /// (`QR|AA`) with one answer, `t14.local A 10.0.2.99`.
    #[test]
    fn a_conflicting_response_received_as_the_probing_ends_takes_the_name() {
        let mut said = vec![0, 0, 0x84, 0x00, 0, 0, 0, 1, 0, 0, 0, 0];
        said.extend_from_slice(b"\x03t14\x05local\x00\x00\x01\x80\x01\x00\x00\x00\x78\x00\x04\x0a\x00\x02\x63");

        let mut m = Machine::new();
        assert_eq!((m.pass(0, true), m.pass(250, true), m.pass(500, true)), (PROBE, PROBE, PROBE));
        m.hears(&said);
        assert_eq!(m.pass(750, true), 0, "no announcement");
        assert_eq!(m.wake_in(750), None, "and nothing more is owed: the name is another host's");
    }
}
