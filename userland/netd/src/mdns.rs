//! This machine's name on its network: `<hostname>.local` answers with the
//! address the lease gave it (`toyos_mdns`, RFC 6762), so another machine on
//! the network reaches it by name with nothing configured on either — the
//! development host finds the T14's served log this way.
//!
//! **Answered only while an address is held**, and announced on every new
//! one. Every decision is `toyos_mdns::Responder`'s; this is the socket and
//! the clock. What the record is owed later — the second announcement (§8.3),
//! or an answer §6 held back — is a wake of netd's own loop
//! ([`Responder::wake_in`]) rather than a sleep, because the protocol names
//! the interval and nothing on the wire says when it has passed.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpCidr, IpEndpoint};
use toyos_mdns::{Asker, Host, Link, To, GROUP, PORT};

/// A query is a few hundred bytes; this holds a handful of them between two
/// passes, and a query past it is dropped by the socket, which is what the
/// asker's own retry is for.
const BUFFER: usize = 4096;

pub struct Responder {
    handle: SocketHandle,
    record: toyos_mdns::Responder<'static>,
    /// The origin of the responder's clock.
    born: Instant,
}

impl Responder {
    /// Join the group and bind its port. `host` is the name this machine asks
    /// its network to record for it (`dhcp::HOSTNAME`).
    pub fn new(host: &'static str, iface: &mut Interface, socket_set: &mut SocketSet<'static>) -> Self {
        let host = Host::new(host).unwrap_or_else(|_| panic!("netd: {host:?} is no host name"));
        iface
            .join_multicast_group(IpAddress::Ipv4(Ipv4Addr::from(GROUP)))
            .expect("netd: the multicast DNS group is the one group this interface joins");
        let buffer = || {
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; BUFFER])
        };
        let mut socket = udp::Socket::new(buffer(), buffer());
        socket.bind(PORT).expect("netd: nothing else binds the multicast DNS port");
        Self { handle: socket_set.add(socket), record: toyos_mdns::Responder::new(host), born: Instant::now() }
    }

    /// After each poll: send what the record is owed now, then answer every
    /// query that arrived.
    pub fn pass(&mut self, iface: &Interface, socket_set: &mut SocketSet<'_>, now: Instant) {
        let socket = socket_set.get_mut::<udp::Socket>(self.handle);
        // IPv4 is the one protocol this netd is built with, so every address is one.
        let link = iface.ip_addrs().first().map(|&IpCidr::Ipv4(cidr)| Link {
            addr: cidr.address().octets(),
            prefix: cidr.prefix_len(),
        });
        let now_ms = self.ms(now);
        let group = IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::from(GROUP)), PORT);
        if let Some(record) = self.record.on(link, now_ms) {
            send(socket, &record, group);
        }
        while let Ok((query, meta)) = socket.recv() {
            let IpAddress::Ipv4(from) = meta.endpoint.addr;
            let asker = Asker { addr: from.octets(), port: meta.endpoint.port };
            let Some(answer) = self.record.answer(query, asker, now_ms) else {
                continue;
            };
            let to = match answer.to {
                To::Group => group,
                To::Asker => meta.endpoint,
            };
            send(socket, &answer.bytes, to);
        }
    }

    /// When the loop must wake for what the record is owed, if anything is.
    pub fn wake_in(&self, now: Instant) -> Option<Duration> {
        self.record.owed_at().map(|at| Duration::from_millis(at.saturating_sub(self.ms(now))))
    }

    fn ms(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.born).as_millis() as u64
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
        Responder { handle, record: toyos_mdns::Responder::new(Host::new("t14").unwrap()), born: Instant::now() }
    }

    /// **A wake is asked for exactly what the record owes.** Nothing before
    /// an address is held; the §8.3 second announcement's own instant once
    /// `on` schedules it. A responder that never asks for this wake answers a
    /// query §6 held back only on some other, unrelated one.
    #[test]
    fn wake_in_asks_for_what_the_record_owes_and_nothing_else() {
        let mut r = responder();
        assert_eq!(r.wake_in(r.born), None, "nothing is owed before an address is held");
        r.record.on(Some(LINK), 0);
        let owed_ms = r.record.owed_at().expect("§8.3 owes the second announcement");
        assert_eq!(r.wake_in(r.born), Some(Duration::from_millis(owed_ms)));
    }
}
