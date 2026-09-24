//! This machine's address, taken from the network rather than written down.
//!
//! What the lease decides is the whole of the interface: the address and its
//! prefix, the default route, and the resolvers the DNS socket queries. All
//! three are written together on every lease and cleared together when one is
//! lost, because a route left standing over an address that is gone sends
//! frames out with a source nothing will answer.
//!
//! **A machine that gets no lease says so and goes on serving.** Its clients
//! then get their connects refused, one refusal at a time, which is what they
//! are already written to survive.

use std::time::{Duration, Instant};

use smoltcp::config::DNS_MAX_SERVER_COUNT;
use smoltcp::iface::Interface;
use smoltcp::socket::{dhcpv4, dns};
use smoltcp::wire::{
    DhcpOption, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr, DHCP_MAX_DNS_SERVER_COUNT,
};

/// **The resolver holds every server a lease can carry.**
/// `dns::Socket::update_servers` truncates to `DNS_MAX_SERVER_COUNT` without
/// saying so and smoltcp's default is one, so this crate asks for
/// `dns-max-server-count-3`; a build that drops it does not compile.
const _: () = assert!(DNS_MAX_SERVER_COUNT >= DHCP_MAX_DNS_SERVER_COUNT);

/// RFC 2132 §3.14.
const OPT_HOST_NAME: u8 = 12;

/// The name this machine asks its network to record for it, and answers to on
/// it as `<name>.local` (`crate::mdns`). One name, because there is one machine.
pub const HOSTNAME: &str = "toyos-t14";

/// The options every DISCOVER and REQUEST carries.
static OUTGOING: [DhcpOption<'static>; 1] =
    [DhcpOption { kind: OPT_HOST_NAME, data: HOSTNAME.as_bytes() }];

/// How long this machine waits for its first lease before saying it has none.
///
/// It bounds the *report*, never the client: the socket retries for the life of
/// the boot and a lease that lands later is applied like any other. What it buys
/// is a line in the log on a machine whose network never answers.
const LEASE_BOUND: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

/// The DHCP client socket this machine runs, asking for a lease under
/// [`HOSTNAME`].
///
/// **Its discovery restarts when the link comes up with no lease held**
/// (`restart`): the client's own retry is ten seconds apart, so a DISCOVER sent
/// into a link still negotiating would otherwise cost the lease that long.
pub fn socket() -> dhcpv4::Socket<'static> {
    let mut socket = dhcpv4::Socket::new();
    socket.set_outgoing_options(&OUTGOING);
    socket
}

/// Ask again from the start, now: the link has just come up and no lease is
/// held.
///
/// **Never on a bound lease.** The client's restart gives the address up
/// before it asks again, so a link that went down and came back would take
/// this machine off its network for a whole exchange. The client offers no
/// way to renew early, so a bound lease is kept across a flap and renews on
/// its own timer.
pub fn restart(client: &mut dhcpv4::Socket) {
    client.reset();
}

/// What the client decided, owned: the resolver this lease writes lives in the
/// same `SocketSet` as the client, so an event still borrowing the client is an
/// event nothing can be done about.
pub enum Change {
    Leased { address: Ipv4Cidr, router: Option<Ipv4Address>, server: Ipv4Address, dns: Vec<Ipv4Address> },
    Lost,
}

impl Change {
    /// Whatever the client has to say this pass.
    pub fn of(client: &mut dhcpv4::Socket) -> Option<Self> {
        match client.poll()? {
            dhcpv4::Event::Configured(config) => Some(Self::Leased {
                address: config.address,
                router: config.router,
                server: config.server.address,
                dns: config.dns_servers.to_vec(),
            }),
            dhcpv4::Event::Deconfigured => Some(Self::Lost),
        }
    }

    /// The address, the router and the server that leased them, where this
    /// change is a lease.
    pub fn lease(&self) -> Option<(Ipv4Cidr, Option<Ipv4Address>, Ipv4Address)> {
        match self {
            Self::Leased { address, router, server, .. } => Some((*address, *router, *server)),
            Self::Lost => None,
        }
    }
}

/// The lease's own state, and what the boot's log still owes about it.
pub struct Dhcp {
    began: Instant,
    /// Whether the interface currently holds a lease.
    leased: bool,
    /// Whether this boot has settled the question once — a lease landed, or the
    /// bound passed with none. netd announces itself on the edge of this.
    settled: bool,
}

impl Dhcp {
    pub fn new() -> Self {
        Self { began: Instant::now(), leased: false, settled: false }
    }

    /// Whether the interface holds a lease now.
    pub fn leased(&self) -> bool {
        self.leased
    }

    /// Apply what the client decided, and answer whether this machine's address
    /// question has just been settled — which is the moment netd has something
    /// to serve with.
    pub fn pass(
        &mut self,
        change: Option<Change>,
        iface: &mut Interface,
        resolver: &mut dns::Socket,
    ) -> bool {
        if let Some(change) = change {
            let (lease, dns) = match change {
                Change::Leased { address, router, server, dns } => {
                    // **One record carrying every field the lease decided.** A
                    // boot read off a stick or a stream has this line and
                    // nothing else to say what this machine's network was.
                    crate::say!(
                        "netd: DHCP: lease {}/{} from {server}, gateway {}, dns [{}], {} ms after \
                         netd came up",
                        address.address(),
                        address.prefix_len(),
                        match router {
                            Some(router) => router.to_string(),
                            None => "none".to_string(),
                        },
                        dns.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "),
                        self.began.elapsed().as_millis(),
                    );
                    (Some((address, router)), dns)
                }
                // Only worth a line where there was something to lose: the
                // client reports this on its way to a first lease too.
                Change::Lost => {
                    if self.leased {
                        crate::say!("netd: DHCP: the lease is gone; this machine has no address");
                    }
                    (None, Vec::new())
                }
            };
            self.leased = lease.is_some();
            Self::write(lease, &dns, iface, resolver);
        }
        if self.settled {
            return false;
        }
        if self.leased {
            self.settled = true;
            return true;
        }
        if self.began.elapsed() >= LEASE_BOUND {
            crate::say!(
                "netd: DHCP: no lease as {} in {} s; this machine has no address and every \
                 connect through it is refused",
                HOSTNAME,
                self.began.elapsed().as_secs(),
            );
            self.settled = true;
            return true;
        }
        false
    }

    /// The address, the default route and the resolvers, written together;
    /// `None` writes the absence of all three. **One writer, reached by every
    /// change**, so a route left standing over an address that is gone cannot
    /// be arranged without breaking the path every boot takes to its lease.
    fn write(
        lease: Option<(Ipv4Cidr, Option<Ipv4Address>)>,
        dns: &[Ipv4Address],
        iface: &mut Interface,
        resolver: &mut dns::Socket,
    ) {
        iface.update_ip_addrs(|addrs| {
            // Cleared before the push, so a list already holding an address
            // cannot leave the old one standing beside the new.
            addrs.clear();
            if let Some((address, _)) = lease {
                addrs.push(IpCidr::Ipv4(address)).expect("an emptied address list takes one");
            }
        });
        iface.routes_mut().remove_default_ipv4_route();
        if let Some(router) = lease.and_then(|(_, router)| router) {
            iface
                .routes_mut()
                .add_default_ipv4_route(router)
                .expect("an emptied route table takes one default route");
        }
        let servers: Vec<IpAddress> = dns.iter().map(|s| IpAddress::Ipv4(*s)).collect();
        resolver.update_servers(&servers);
    }
}
