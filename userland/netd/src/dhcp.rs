//! This machine's address, taken from the network rather than written down.
//!
//! **There is no static configuration to fall back to.** A machine's address
//! belongs to the network it is plugged into, and both networks this program
//! has ever run on — QEMU's user-mode backend and the bench's router — serve
//! DHCP. What a hard-coded `10.0.2.15/24` bought was one of them, and it bought
//! it by being right about a machine nobody had asked.
//!
//! What the lease decides is the whole of the interface: the address and its
//! prefix, the default route, and the resolvers the DNS socket queries. All
//! three are replaced together on every lease and dropped together when one is
//! lost, because a route left standing over an address that is gone sends
//! frames out with a source nothing will answer.
//!
//! **A machine that gets no lease says so and goes on serving.** Its clients
//! then get their connects refused, one refusal at a time, which is what they
//! are already written to survive; a daemon that waited here instead would put
//! a whole userland behind a router that did not answer.

use std::time::{Duration, Instant};

use smoltcp::iface::Interface;
use smoltcp::socket::{dhcpv4, dns};
use smoltcp::wire::{DhcpOption, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};

/// RFC 2132 §3.14.
const OPT_HOST_NAME: u8 = 12;

/// The name this machine asks its network to record for it.
///
/// **One name, because there is one machine.** The bench's router is the only
/// DHCP server in reach that records a client's name at all, and what it
/// records this one under is what `toyos-t14` then resolves to — so the name is
/// the bench's, and a second machine running this program would need a second
/// answer before it needed anything else here.
const HOSTNAME: &[u8] = b"toyos-t14";

/// The options every DISCOVER and REQUEST carries.
static OUTGOING: [DhcpOption<'static>; 1] =
    [DhcpOption { kind: OPT_HOST_NAME, data: HOSTNAME }];

/// How long this machine waits for its first lease before saying it has none.
///
/// It bounds the *report*, never the client: the socket goes on retrying for
/// the life of the boot, and a lease that lands after this is applied like any
/// other. What the bound buys is a line in the log on a machine whose network
/// never answers, instead of a boot that is silent about the one thing wrong
/// with it. Wide enough for a gigabit link to finish negotiating first, which
/// on the bench's I219 is seconds.
const LEASE_BOUND: Duration = Duration::from_secs(20);

/// The DHCP client socket this machine runs, asking for a lease under
/// [`HOSTNAME`].
pub fn socket() -> dhcpv4::Socket<'static> {
    let mut socket = dhcpv4::Socket::new();
    socket.set_outgoing_options(&OUTGOING);
    socket
}

/// What the client decided, owned.
///
/// **Taken out of the socket before anything is applied**, because the
/// interface and the DNS resolver are the other two things a lease changes and
/// all three live in one `SocketSet`: an event still borrowing the client is an
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

    /// How long netd may sleep before this owes the log a line.
    ///
    /// **A bound nothing else would wake for.** A machine whose network never
    /// answers produces no frame and no timer, so the loop's own delay is
    /// unbounded and the report at [`LEASE_BOUND`] would never be written.
    pub fn report_within(&self) -> Option<Duration> {
        (!self.settled).then(|| LEASE_BOUND.saturating_sub(self.began.elapsed()))
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
        match change {
            Some(Change::Leased { address, router, server, dns }) => {
                self.apply(address, router, server, &dns, iface, resolver);
                self.leased = true;
            }
            Some(Change::Lost) => {
                // Only worth a line where there was something to lose: the
                // client reports this on its way to a first lease too.
                if self.leased {
                    crate::say!("netd: DHCP: the lease is gone; this machine has no address");
                }
                self.clear(iface, resolver);
                self.leased = false;
            }
            None => {}
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
                String::from_utf8_lossy(HOSTNAME),
                LEASE_BOUND.as_secs(),
            );
            self.settled = true;
            return true;
        }
        false
    }

    /// The lease, written into the interface and said out loud.
    ///
    /// **One record carrying every field the lease decided.** A boot read off a
    /// stick or a stream has this line and nothing else to say what this
    /// machine's network was, and a judge that had to assemble it from three
    /// lines would be guessing which boot each of them came from.
    fn apply(
        &self,
        address: Ipv4Cidr,
        router: Option<Ipv4Address>,
        server: Ipv4Address,
        dns: &[Ipv4Address],
        iface: &mut Interface,
        resolver: &mut dns::Socket,
    ) {
        iface.update_ip_addrs(|addrs| {
            // Cleared before the push, so a list already holding an address
            // cannot leave the old one standing beside the new.
            addrs.clear();
            addrs.push(IpCidr::Ipv4(address)).expect("an emptied address list takes one");
        });
        iface.routes_mut().remove_default_ipv4_route();
        if let Some(router) = router {
            iface
                .routes_mut()
                .add_default_ipv4_route(router)
                .expect("an emptied route table takes one default route");
        }
        let servers: Vec<IpAddress> = dns.iter().map(|s| IpAddress::Ipv4(*s)).collect();
        resolver.update_servers(&servers);
        crate::say!(
            "netd: DHCP: lease {}/{} from {server}, gateway {}, dns [{}], {} ms after netd came up",
            address.address(),
            address.prefix_len(),
            match router {
                Some(router) => router.to_string(),
                None => "none".to_string(),
            },
            dns.iter().map(ToString::to_string).collect::<Vec<_>>().join(" "),
            self.began.elapsed().as_millis(),
        );
    }

    fn clear(&self, iface: &mut Interface, resolver: &mut dns::Socket) {
        iface.update_ip_addrs(|addrs| addrs.clear());
        iface.routes_mut().remove_default_ipv4_route();
        resolver.update_servers(&[]);
    }
}
