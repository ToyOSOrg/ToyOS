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

pub struct Responder {
    handle: SocketHandle,
    host: &'static str,
    record: toyos_mdns::Responder<'static>,
    /// The origin of the responder's clock.
    born: Instant,
}

impl Responder {
    /// Join the group and bind its port. `host` is the name this machine asks
    /// its network to record for it (`dhcp::HOSTNAME`).
    pub fn new(host: &'static str, iface: &mut Interface, socket_set: &mut SocketSet<'static>) -> Self {
        let label = Host::new(host).unwrap_or_else(|_| panic!("netstack: {host:?} is no host name"));
        iface
            .join_multicast_group(IpAddress::Ipv4(Ipv4Addr::from(GROUP)))
            .expect("netstack: the multicast DNS group is the one group this interface joins");
        let buffer = || {
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; BUFFER])
        };
        let mut socket = udp::Socket::new(buffer(), buffer());
        socket.bind(PORT).expect("netstack: nothing else binds the multicast DNS port");
        Self { handle: socket_set.add(socket), host, record: toyos_mdns::Responder::new(label), born: Instant::now() }
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
        let (owed, event) = self.record.owed(now_ms, || u32::from(crate::resolve::random_u16()));
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
    use smoltcp::iface::SocketSet;

    const LINK: Link = Link { addr: [10, 0, 2, 15], prefix: 24 };

    /// A `Responder` over a socket taken from a set of its own — `wake_in`
    /// touches neither, only `record` and `born`.
    fn responder() -> Responder {
        let buffer = || udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY], vec![0u8; 64]);
        let mut set = SocketSet::new(Vec::new());
        let handle = set.add(udp::Socket::new(buffer(), buffer()));
        Responder { handle, host: "t14", record: toyos_mdns::Responder::new(Host::new("t14").unwrap()), born: Instant::now() }
    }

    /// **A wake is asked for exactly what the name owes.** Nothing before an
    /// address is held; the first probe's own instant, the drawn delay after
    /// the address, once the record schedules it. A responder that never asks
    /// for this wake sends that probe, and every one after it, only on some
    /// other, unrelated wake.
    #[test]
    fn wake_in_asks_for_what_the_name_owes_and_nothing_else() {
        let mut r = responder();
        assert_eq!(r.wake_in(r.born), None, "nothing is owed before an address is held");
        r.record.on(Some(LINK), 0);
        assert_eq!(r.record.owed(0, || 100), (None, None), "§8.1: the delay before the first probe");
        assert_eq!(r.wake_in(r.born), Some(Duration::from_millis(100)));
        assert_eq!(r.wake_in(r.born + Duration::from_millis(40)), Some(Duration::from_millis(60)));
    }
}
