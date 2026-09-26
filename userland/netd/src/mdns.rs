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
//!
//! Sent with an IP TTL of 255, which §11 asks every multicast DNS sender for.

use std::num::{NonZeroU16, NonZeroU8};
use std::time::{Duration, Instant};

use net_types::ip::{Ipv4, Ipv4Addr};
use net_types::{MulticastAddr, SpecifiedAddr, ZonedAddr};
use netstack3_core::socket::{MulticastInterfaceSelector, MulticastMembershipInterfaceSelector};
use netstack3_core::udp::UdpRemotePort;
use packet::Buf;
use toyos_mdns::{Asker, Host, Link, To, GROUP, PORT};

use crate::net::Net;
use crate::resolve::UdpId;
use crate::stack::Inbox;

pub struct Responder {
    socket: UdpId,
    record: toyos_mdns::Responder<'static>,
    /// The origin of the responder's clock.
    born: Instant,
}

impl Responder {
    /// Join the group and bind its port. `host` is the name this machine asks
    /// its network to record for it (`dhcp::HOSTNAME`).
    pub fn new(host: &'static str, net: &mut Net) -> Self {
        let host = Host::new(host).unwrap_or_else(|_| panic!("netd: {host:?} is no host name"));
        let device = net.device_id();
        let group = MulticastAddr::new(Ipv4Addr::new(GROUP)).expect("the multicast DNS group is multicast");
        let api = net.api();
        let mut udp = api.udp::<Ipv4>();
        let socket = udp.create_with(Inbox::default());
        udp.listen(&socket, None, NonZeroU16::new(PORT))
            .unwrap_or_else(|e| panic!("netd: nothing else binds the multicast DNS port, and it refused: {e:?}"));
        udp.set_multicast_membership(
            &socket,
            group,
            MulticastMembershipInterfaceSelector::Specified(MulticastInterfaceSelector::Interface(device.clone())),
            true,
        )
        .unwrap_or_else(|e| panic!("netd: its device refused the multicast DNS group: {e:?}"));
        udp.set_multicast_interface(&socket, Some(&device), net_types::ip::IpVersion::V4)
            .unwrap_or_else(|e| panic!("netd: its socket refused its device for multicast: {e:?}"));
        let ttl = NonZeroU8::new(255).expect("255 is a hop limit");
        udp.set_multicast_hop_limit(&socket, Some(ttl), net_types::ip::IpVersion::V4)
            .unwrap_or_else(|e| panic!("netd: its socket refused a multicast TTL: {e:?}"));
        udp.set_unicast_hop_limit(&socket, Some(ttl), net_types::ip::IpVersion::V4)
            .unwrap_or_else(|e| panic!("netd: its socket refused a unicast TTL: {e:?}"));
        Self { socket, record: toyos_mdns::Responder::new(host), born: Instant::now() }
    }

    /// After each poll: send what the record is owed now, then answer every
    /// query that arrived.
    pub fn pass(&mut self, net: &mut Net, now: Instant) {
        let link = net.address().map(|a| Link { addr: a.addr, prefix: a.prefix });
        let now_ms = self.ms(now);
        if let Some(record) = self.record.on(link, now_ms) {
            send(net, &self.socket, &record, GROUP, PORT);
        }
        while let Some(query) = self.socket.external_data().take() {
            let asker = Asker { addr: query.from, port: query.port };
            let Some(answer) = self.record.answer(&query.bytes, asker, now_ms) else {
                continue;
            };
            let (to, port) = match answer.to {
                To::Group => (GROUP, PORT),
                To::Asker => (query.from, query.port),
            };
            send(net, &self.socket, &answer.bytes, to, port);
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

/// Send `bytes` to `to`. A refusal — an asker's own source that nothing on
/// the wire reaches, or no route left — drops the answer, which the asker's
/// own retry is for; nothing here waits.
fn send(net: &mut Net, socket: &UdpId, bytes: &[u8], to: [u8; 4], port: u16) {
    let (Some(to), Some(port)) = (SpecifiedAddr::new(Ipv4Addr::new(to)), NonZeroU16::new(port)) else {
        return;
    };
    let _ = net.api().udp::<Ipv4>().send_to(
        socket,
        Some(ZonedAddr::Unzoned(to)),
        UdpRemotePort::Set(port),
        Buf::new(bytes.to_vec(), ..),
        (),
    );
}
