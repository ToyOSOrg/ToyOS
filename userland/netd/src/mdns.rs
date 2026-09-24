//! This machine's name on its network: `<hostname>.local` answers with the
//! address the lease gave it (`toyos_mdns`, RFC 6762), so another machine on
//! the network reaches it by name with nothing configured on either — the
//! development host finds the T14's served log this way.
//!
//! **Answered only while an address is held**, and announced on every new
//! one: RFC 6762 §8.3 asks for two unsolicited answers one second apart, and
//! the second is a wake of netd's own loop ([`Responder::wake_in`]) rather than
//! a sleep, because the protocol names the interval and nothing on the wire
//! says when it has passed.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint};
use toyos_mdns::{Host, To, GROUP, PORT};

/// §8.3: "The Multicast DNS responder MUST send at least two unsolicited
/// responses, one second apart."
const ANNOUNCE_AGAIN: Duration = Duration::from_secs(1);

/// A query is a few hundred bytes; this holds a handful of them between two
/// passes, and a query past it is dropped by the socket, which is what the
/// asker's own retry is for.
const BUFFER: usize = 4096;

pub struct Responder {
    handle: SocketHandle,
    host: Host<'static>,
    /// The address last announced, and when its second announcement is owed.
    announced: Option<Ipv4Addr>,
    again_at: Option<Instant>,
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
        Self { handle: socket_set.add(socket), host, announced: None, again_at: None }
    }

    /// After each poll: answer every query that arrived, and announce an
    /// address that is new or owed its second announcement.
    pub fn pass(&mut self, iface: &Interface, socket_set: &mut SocketSet<'_>, now: Instant) {
        let socket = socket_set.get_mut::<udp::Socket>(self.handle);
        let Some(addr) = iface.ipv4_addr() else {
            // No address, so nothing to answer with: the queries are read and
            // let go, and the next address is announced as new.
            while socket.recv().is_ok() {}
            self.announced = None;
            self.again_at = None;
            return;
        };
        let group = IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::from(GROUP)), PORT);
        if self.announced != Some(addr) {
            self.announced = Some(addr);
            self.again_at = Some(now + ANNOUNCE_AGAIN);
            send(socket, &toyos_mdns::announcement(self.host, addr.octets()), group);
        } else if self.again_at.is_some_and(|at| now >= at) {
            self.again_at = None;
            send(socket, &toyos_mdns::announcement(self.host, addr.octets()), group);
        }
        while let Ok((query, meta)) = socket.recv() {
            let Some(answer) = toyos_mdns::answer(query, meta.endpoint.port, self.host, addr.octets())
            else {
                continue;
            };
            let to = match answer.to {
                To::Group => group,
                To::Asker => meta.endpoint,
            };
            send(socket, &answer.bytes, to);
        }
    }

    /// When the loop must wake for the second announcement, if one is owed.
    pub fn wake_in(&self, now: Instant) -> Option<Duration> {
        self.again_at.map(|at| at.saturating_duration_since(now))
    }
}

/// A full send buffer is a burst of queries this pass cannot answer; the
/// asker retries, and nothing here waits.
fn send(socket: &mut udp::Socket, bytes: &[u8], to: IpEndpoint) {
    let _ = socket.send_slice(bytes, to);
}
