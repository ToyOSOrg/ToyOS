//! This machine's address, taken from the network rather than written down: a
//! DHCPv4 client (RFC 2131), its own frames in and out.
//!
//! **The client speaks below the stack**, as every DHCP client does before it
//! has an address: netd hands it each frame for UDP port 68 before the stack
//! sees any ([`Client::takes`]), and sends the frames it makes as they are, so
//! nothing it does depends on the stack holding an address, a route or a
//! neighbour. A renewal goes to the link address the lease's acknowledgement
//! came from.
//!
//! What the lease decides is the whole of the interface — the address and its
//! prefix, the routes, and the servers `crate::resolve` asks — and netd writes
//! all three together on every [`Change`].
//!
//! **Timing is RFC 2131's**: a DISCOVER or a REQUEST unanswered is sent again
//! after 4 s, doubled to 64 s, each randomised by ±1 s (§4.1); REQUESTING
//! unanswered past that backoff starts over (§3.1); a bound lease renews at T1
//! and rebinds at T2 — the server's, or half and seven eighths of the lease
//! (§4.4.5) — asking again half the time left, at least 60 s, apart; and a
//! lease that reaches its end unextended is lost.
//!
//! Not done: the ARP probe of an offered address and the DECLINE it would
//! send (RFC 2131 §2.2, a SHOULD), and INIT-REBOOT of a remembered address.

use std::time::{Duration, Instant};

/// The name this machine asks its network to record for it, and answers to on
/// it as `<name>.local` (`crate::mdns`). One name, because there is one machine.
pub const HOSTNAME: &str = "toyos-t14";

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
const MAGIC: [u8; 4] = [99, 130, 83, 99];

// RFC 2132 option codes.
const OPT_PAD: u8 = 0;
const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_HOST_NAME: u8 = 12;
const OPT_REQUESTED_ADDRESS: u8 = 50;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAMETERS: u8 = 55;
const OPT_MAX_MESSAGE: u8 = 57;
const OPT_RENEWAL_TIME: u8 = 58;
const OPT_REBINDING_TIME: u8 = 59;
const OPT_END: u8 = 255;

// RFC 2132 §9.6 message types.
const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

/// The most resolvers a lease is kept with: the first three it names.
pub const MAX_DNS: usize = 3;

/// The first retransmission's wait and the last's (RFC 2131 §4.1).
const FIRST_WAIT: Duration = Duration::from_secs(4);
const LAST_WAIT: Duration = Duration::from_secs(64);

/// The least a RENEWING or REBINDING client waits between two REQUESTs
/// (RFC 2131 §4.4.5).
const RENEW_FLOOR: Duration = Duration::from_secs(60);

/// A lease, as the server decided it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: [u8; 4],
    pub prefix: u8,
    pub router: Option<[u8; 4]>,
    pub server: [u8; 4],
    pub dns: Vec<[u8; 4]>,
    /// The link address the lease came from, which a renewal is sent to.
    server_mac: [u8; 6],
    duration: Duration,
    renew: Duration,
    rebind: Duration,
}

/// What the client decided, for netd to write into the interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Leased(Lease),
    Lost,
}

enum State {
    /// Asking for offers.
    Selecting,
    /// Asking the server that offered `address` for it.
    Requesting { address: [u8; 4], server: [u8; 4] },
    /// Holding `lease`, acknowledged at `at`.
    Bound { lease: Lease, at: Instant },
    /// Asking the lease's server to extend it.
    Renewing { lease: Lease, at: Instant },
    /// Asking any server to extend it.
    Rebinding { lease: Lease, at: Instant },
}

/// One DHCPv4 client.
pub struct Client {
    mac: [u8; 6],
    state: State,
    /// The exchange's transaction id.
    xid: u32,
    /// When the exchange began, which `secs` counts from.
    began: Instant,
    /// When the client is next owed the time.
    due: Instant,
    /// The wait before the next retransmission of a DISCOVER or a selecting
    /// REQUEST.
    wait: Duration,
    draw: fn() -> u32,
    /// Frames made and not yet taken by the loop.
    out: Vec<Vec<u8>>,
}

impl Client {
    /// A client that sends its first DISCOVER at `now`. `draw` is the random
    /// source every transaction id and randomised wait comes from.
    pub fn new(mac: [u8; 6], now: Instant, draw: fn() -> u32) -> Self {
        let mut client = Self {
            mac,
            state: State::Selecting,
            xid: 0,
            began: now,
            due: now,
            wait: FIRST_WAIT,
            draw,
            out: Vec::new(),
        };
        client.exchange(now);
        client
    }

    /// Start over from discovery, now: a link just up with no lease held.
    /// **Never over a held lease**, which a link that flapped keeps.
    pub fn restart(&mut self, now: Instant) {
        assert!(self.lease().is_none(), "netd: DHCP restarted over a held lease");
        self.state = State::Selecting;
        self.exchange(now);
    }

    /// The lease held, whatever state it is being extended in.
    pub fn lease(&self) -> Option<&Lease> {
        match &self.state {
            State::Bound { lease, .. } | State::Renewing { lease, .. } | State::Rebinding { lease, .. } => {
                Some(lease)
            }
            State::Selecting | State::Requesting { .. } => None,
        }
    }

    /// When the client must next be given the time.
    pub fn wake_at(&self) -> Instant {
        self.due
    }

    /// The frames made since the last call.
    pub fn take_frames(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.out)
    }

    /// Whether `frame` is the client's: IPv4, UDP, to port 68.
    pub fn takes(frame: &[u8]) -> bool {
        headers(frame).is_some_and(|(_, dport, _)| dport == CLIENT_PORT)
    }

    /// Act on the time: send what is owed, and give the lease up at its end.
    pub fn on_time(&mut self, now: Instant) -> Option<Change> {
        if now < self.due {
            return None;
        }
        let state = std::mem::replace(&mut self.state, State::Selecting);
        let (state, change) = match state {
            State::Selecting => {
                self.discover(now);
                (State::Selecting, None)
            }
            State::Requesting { .. } if self.wait > LAST_WAIT => {
                self.exchange(now);
                self.discover(now);
                (State::Selecting, None)
            }
            State::Requesting { address, server } => {
                self.select(now, address, server);
                (State::Requesting { address, server }, None)
            }
            State::Bound { lease, at } if now >= at + lease.rebind => {
                self.begin(now);
                self.rebind(now, lease, at)
            }
            State::Bound { lease, at } => {
                self.begin(now);
                self.renew(now, lease, at)
            }
            State::Renewing { lease, at } if now >= at + lease.rebind => self.rebind(now, lease, at),
            State::Renewing { lease, at } => self.renew(now, lease, at),
            State::Rebinding { lease, at } => self.rebind(now, lease, at),
        };
        self.state = state;
        change
    }

    /// Read a frame [`Client::takes`], and act on it if it answers this
    /// client's exchange.
    pub fn on_frame(&mut self, frame: &[u8], now: Instant) -> Option<Change> {
        let reply = Reply::parse(frame, self.mac, self.xid)?;
        let state = std::mem::replace(&mut self.state, State::Selecting);
        let (state, change) = match (state, reply.kind) {
            (State::Selecting, OFFER) if reply.server.is_some() => {
                let (address, server) = (reply.yiaddr, reply.server.expect("checked"));
                self.wait = FIRST_WAIT;
                self.select(now, address, server);
                (State::Requesting { address, server }, None)
            }
            (State::Requesting { address, server }, ACK) if reply.yiaddr == address && reply.server == Some(server) => {
                match reply.lease() {
                    Some(lease) => self.bind(lease, now),
                    None => (State::Requesting { address, server }, None),
                }
            }
            (State::Requesting { server, .. }, NAK) if reply.server == Some(server) => {
                self.exchange(now);
                (State::Selecting, None)
            }
            (State::Renewing { lease, .. } | State::Rebinding { lease, .. }, ACK)
                if reply.yiaddr == lease.address && reply.lease().is_some() =>
            {
                self.bind(reply.lease().expect("checked"), now)
            }
            (State::Renewing { .. } | State::Rebinding { .. }, NAK) => {
                self.exchange(now);
                (State::Selecting, Some(Change::Lost))
            }
            (state, _) => (state, None),
        };
        self.state = state;
        change
    }

    fn bind(&mut self, lease: Lease, now: Instant) -> (State, Option<Change>) {
        self.due = now + lease.renew;
        (State::Bound { lease: lease.clone(), at: now }, Some(Change::Leased(lease)))
    }

    /// RENEWING: a unicast REQUEST to the lease's server, again half the time
    /// to T2 later.
    fn renew(&mut self, now: Instant, lease: Lease, at: Instant) -> (State, Option<Change>) {
        let t2 = at + lease.rebind;
        self.request(now, lease.address, lease.server_mac, lease.server);
        self.due = (now + (t2.saturating_duration_since(now) / 2).max(RENEW_FLOOR)).min(t2);
        (State::Renewing { lease, at }, None)
    }

    /// REBINDING: a broadcast REQUEST, again half the lease left later, and
    /// the lease gone at its end.
    fn rebind(&mut self, now: Instant, lease: Lease, at: Instant) -> (State, Option<Change>) {
        let end = at + lease.duration;
        if now >= end {
            self.exchange(now);
            self.discover(now);
            return (State::Selecting, Some(Change::Lost));
        }
        self.request(now, lease.address, [0xff; 6], [255; 4]);
        self.due = (now + (end.saturating_duration_since(now) / 2).max(RENEW_FLOOR)).min(end);
        (State::Rebinding { lease, at }, None)
    }

    /// A fresh exchange from discovery, its DISCOVER owed now.
    fn exchange(&mut self, now: Instant) {
        self.begin(now);
        self.wait = FIRST_WAIT;
        self.due = now;
    }

    /// A fresh transaction id and `secs` origin.
    fn begin(&mut self, now: Instant) {
        self.xid = (self.draw)();
        self.began = now;
    }

    fn discover(&mut self, now: Instant) {
        let mut options = Options::new(DISCOVER);
        options.common();
        let bootp = self.bootp(now, [0; 4], &options.0);
        self.out.push(frame(self.mac, [0xff; 6], [0; 4], [255; 4], &bootp));
        self.backoff(now);
    }

    /// SELECTING's REQUEST: broadcast, naming the address and its server.
    fn select(&mut self, now: Instant, address: [u8; 4], server: [u8; 4]) {
        let mut options = Options::new(REQUEST);
        options.put(OPT_REQUESTED_ADDRESS, &address);
        options.put(OPT_SERVER_ID, &server);
        options.common();
        let bootp = self.bootp(now, [0; 4], &options.0);
        self.out.push(frame(self.mac, [0xff; 6], [0; 4], [255; 4], &bootp));
        self.backoff(now);
    }

    /// RENEWING's or REBINDING's REQUEST: from the address held, naming it in
    /// `ciaddr` and nowhere else (RFC 2131 §4.3.2).
    fn request(&mut self, now: Instant, held: [u8; 4], to_mac: [u8; 6], to: [u8; 4]) {
        let mut options = Options::new(REQUEST);
        options.common();
        let bootp = self.bootp(now, held, &options.0);
        self.out.push(frame(self.mac, to_mac, held, to, &bootp));
    }

    /// The next retransmission: this wait ±1 s, and the next wait doubled.
    fn backoff(&mut self, now: Instant) {
        let jitter = Duration::from_millis(u64::from((self.draw)() % 2001));
        self.due = now + self.wait - Duration::from_secs(1) + jitter;
        self.wait *= 2;
    }

    /// A BOOTREQUEST (RFC 2131 §2) carrying `options`.
    fn bootp(&self, now: Instant, ciaddr: [u8; 4], options: &[u8]) -> Vec<u8> {
        let mut m = vec![0u8; 236];
        m[0] = 1; // BOOTREQUEST
        m[1] = 1; // Ethernet
        m[2] = 6;
        m[4..8].copy_from_slice(&self.xid.to_be_bytes());
        let secs = u16::try_from(now.saturating_duration_since(self.began).as_secs()).unwrap_or(u16::MAX);
        m[8..10].copy_from_slice(&secs.to_be_bytes());
        m[12..16].copy_from_slice(&ciaddr);
        m[28..34].copy_from_slice(&self.mac);
        m.extend_from_slice(&MAGIC);
        m.extend_from_slice(options);
        m
    }
}

/// A message's options, being written.
struct Options(Vec<u8>);

impl Options {
    fn new(kind: u8) -> Self {
        let mut o = Self(Vec::new());
        o.put(OPT_MESSAGE_TYPE, &[kind]);
        o
    }

    fn put(&mut self, code: u8, data: &[u8]) {
        self.0.push(code);
        self.0.push(u8::try_from(data.len()).expect("an option's data fits its length byte"));
        self.0.extend_from_slice(data);
    }

    /// What every DISCOVER and REQUEST carries, and the end.
    fn common(&mut self) {
        self.put(OPT_HOST_NAME, HOSTNAME.as_bytes());
        self.put(OPT_MAX_MESSAGE, &1500u16.to_be_bytes());
        self.put(
            OPT_PARAMETERS,
            &[OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS, OPT_LEASE_TIME, OPT_RENEWAL_TIME, OPT_REBINDING_TIME],
        );
        self.0.push(OPT_END);
    }
}

/// A server's answer to this client, as far as the client reads one.
struct Reply {
    kind: u8,
    yiaddr: [u8; 4],
    server: Option<[u8; 4]>,
    server_mac: [u8; 6],
    mask: Option<[u8; 4]>,
    router: Option<[u8; 4]>,
    dns: Vec<[u8; 4]>,
    lease: Option<u32>,
    renew: Option<u32>,
    rebind: Option<u32>,
}

impl Reply {
    /// `frame` as a BOOTREPLY to `mac`'s transaction `xid`, or `None` where it
    /// is not one: any other datagram to port 68 is some other client's.
    fn parse(frame: &[u8], mac: [u8; 6], xid: u32) -> Option<Self> {
        let (_, dport, m) = headers(frame)?;
        if dport != CLIENT_PORT || m.len() < 240 || m[0] != 2 || m[1] != 1 || m[2] != 6 {
            return None;
        }
        if m[4..8] != xid.to_be_bytes() || m[28..34] != mac || m[236..240] != MAGIC {
            return None;
        }
        let mut reply = Self {
            kind: 0,
            yiaddr: m[16..20].try_into().expect("four bytes"),
            server: None,
            server_mac: frame[6..12].try_into().expect("six bytes"),
            mask: None,
            router: None,
            dns: Vec::new(),
            lease: None,
            renew: None,
            rebind: None,
        };
        let mut options = &m[240..];
        while let Some((&code, rest)) = options.split_first() {
            match code {
                OPT_PAD => {
                    options = rest;
                    continue;
                }
                OPT_END => break,
                _ => {}
            }
            let (&len, rest) = rest.split_first()?;
            let data = rest.get(..usize::from(len))?;
            options = &rest[usize::from(len)..];
            let addr = |d: &[u8]| -> Option<[u8; 4]> { d.get(..4)?.try_into().ok() };
            let secs = |d: &[u8]| -> Option<u32> { Some(u32::from_be_bytes(d.get(..4)?.try_into().ok()?)) };
            match code {
                OPT_MESSAGE_TYPE => reply.kind = *data.first()?,
                OPT_SERVER_ID => reply.server = addr(data),
                OPT_SUBNET_MASK => reply.mask = addr(data),
                OPT_ROUTER => reply.router = addr(data),
                OPT_DNS => {
                    reply.dns = data.chunks_exact(4).take(MAX_DNS).map(|d| d.try_into().expect("four")).collect()
                }
                OPT_LEASE_TIME => reply.lease = secs(data),
                OPT_RENEWAL_TIME => reply.renew = secs(data),
                OPT_REBINDING_TIME => reply.rebind = secs(data),
                _ => {}
            }
        }
        let unicast = reply.yiaddr != [0; 4] && reply.yiaddr[0] < 224;
        (reply.kind != 0 && (reply.kind == NAK || unicast)).then_some(reply)
    }

    /// The lease an ACK grants, or `None` where it is not whole: an ACK with no
    /// mask, no lease time or no server id leaves this machine nothing it can
    /// configure, and is ignored like any stray reply (RFC 2131 §4.3.1 has the
    /// server send all three).
    fn lease(&self) -> Option<Lease> {
        let mask = u32::from_be_bytes(self.mask?);
        if mask == 0 || mask.leading_ones() + mask.trailing_zeros() != 32 {
            return None;
        }
        let duration = Duration::from_secs(u64::from(self.lease?));
        let renew = self.renew.map_or(duration / 2, |s| Duration::from_secs(u64::from(s))).min(duration);
        let rebind = self.rebind.map_or(duration * 7 / 8, |s| Duration::from_secs(u64::from(s))).clamp(renew, duration);
        Some(Lease {
            address: self.yiaddr,
            prefix: mask.leading_ones() as u8,
            router: self.router.filter(|r| *r != [0; 4]),
            server: self.server?,
            dns: self.dns.clone(),
            server_mac: self.server_mac,
            duration,
            renew,
            rebind,
        })
    }
}

/// An Ethernet frame's IPv4 source, UDP destination port and UDP payload.
/// `None` for anything else, a fragment, or a header whose checksum does not
/// hold.
fn headers(frame: &[u8]) -> Option<([u8; 4], u16, &[u8])> {
    if frame.get(12..14)? != [0x08, 0x00] {
        return None;
    }
    let ip = &frame[14..];
    let ihl = usize::from(*ip.first()? & 0x0f) * 4;
    if ip[0] >> 4 != 4 || ihl < 20 || ip.len() < ihl || ip[9] != 17 {
        return None;
    }
    let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    let fragment = u16::from_be_bytes([ip[6], ip[7]]) & 0x3fff;
    if total < ihl + 8 || total > ip.len() || fragment != 0 || checksum(&[&ip[..ihl]]) != 0 {
        return None;
    }
    let src: [u8; 4] = ip[12..16].try_into().expect("four");
    let dst: [u8; 4] = ip[16..20].try_into().expect("four");
    let udp = &ip[ihl..total];
    let len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
    if len < 8 || len > udp.len() {
        return None;
    }
    let udp = &udp[..len];
    if udp[6..8] != [0, 0] && checksum(&[&pseudo(src, dst, len), udp]) != 0 {
        return None;
    }
    Some((src, u16::from_be_bytes([udp[2], udp[3]]), &udp[8..]))
}

/// The UDP pseudo-header (RFC 768).
fn pseudo(src: [u8; 4], dst: [u8; 4], len: usize) -> [u8; 12] {
    let mut p = [0u8; 12];
    p[..4].copy_from_slice(&src);
    p[4..8].copy_from_slice(&dst);
    p[9] = 17;
    p[10..].copy_from_slice(&u16::try_from(len).expect("a datagram's length").to_be_bytes());
    p
}

/// The Internet checksum (RFC 1071) over `parts`, each of even length but the
/// last.
fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum = 0u32;
    for part in parts {
        let mut chunks = part.chunks_exact(2);
        for c in &mut chunks {
            sum += u32::from(u16::from_be_bytes([c[0], c[1]]));
        }
        if let [last] = chunks.remainder() {
            sum += u32::from(*last) << 8;
        }
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// An Ethernet frame carrying `bootp` from port 68 to port 67.
fn frame(mac: [u8; 6], to_mac: [u8; 6], from: [u8; 4], to: [u8; 4], bootp: &[u8]) -> Vec<u8> {
    let udp_len = 8 + bootp.len();
    let total = 20 + udp_len;
    let mut f = Vec::with_capacity(14 + total);
    f.extend_from_slice(&to_mac);
    f.extend_from_slice(&mac);
    f.extend_from_slice(&[0x08, 0x00]);
    let mut ip = [0u8; 20];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&u16::try_from(total).expect("a frame's length").to_be_bytes());
    ip[8] = 64;
    ip[9] = 17;
    ip[12..16].copy_from_slice(&from);
    ip[16..20].copy_from_slice(&to);
    let sum = checksum(&[&ip]);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    f.extend_from_slice(&ip);
    let mut udp = Vec::with_capacity(udp_len);
    udp.extend_from_slice(&CLIENT_PORT.to_be_bytes());
    udp.extend_from_slice(&SERVER_PORT.to_be_bytes());
    udp.extend_from_slice(&u16::try_from(udp_len).expect("a datagram's length").to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(bootp);
    // RFC 768: a computed zero is sent as all ones.
    let sum = match checksum(&[&pseudo(from, to, udp_len), &udp]) {
        0 => 0xffff,
        sum => sum,
    };
    udp[6..8].copy_from_slice(&sum.to_be_bytes());
    f.extend_from_slice(&udp);
    f
}

/// How long this machine waits for its first lease before saying it has none.
///
/// It bounds the *report*, never the client: the client asks for the life of
/// the boot and a lease that lands later is applied like any other. What it
/// buys is a line in the log on a machine whose network never answers.
const LEASE_BOUND: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

/// The client, and what the boot's log still owes about it.
pub struct Dhcp {
    pub client: Client,
    began: Instant,
    /// Whether this boot has settled the question once — a lease landed, or the
    /// bound passed with none. netd announces itself on the edge of this.
    settled: bool,
}

impl Dhcp {
    pub fn new(mac: [u8; 6], now: Instant, draw: fn() -> u32) -> Self {
        Self { client: Client::new(mac, now, draw), began: now, settled: false }
    }

    /// Whether a lease is held now.
    pub fn leased(&self) -> bool {
        self.client.lease().is_some()
    }

    /// The lease as `inspect` reads it: what the interface holds now.
    pub fn inspect(&self, snap: &mut toyos_inspect::Snapshot) {
        let Some(lease) = self.client.lease() else {
            snap.put("lease.held", false);
            return;
        };
        snap.put("lease.held", true);
        snap.put("lease.address", format!("{}/{}", show(lease.address), lease.prefix));
        snap.put("lease.server", show(lease.server));
        if let Some(router) = lease.router {
            snap.put("lease.router", show(router));
        }
        let dns: Vec<String> = lease.dns.iter().map(|d| show(*d)).collect();
        snap.put("lease.dns", dns.join(" "));
    }

    /// The one line a lease is recorded with: every field the lease decided,
    /// since a boot read off a stick or a stream has this line and nothing
    /// else to say what this machine's network was.
    pub fn say_leased(&self, lease: &Lease, now: Instant) {
        crate::say!(
            "netd: DHCP: lease {}/{} from {}, gateway {}, dns [{}], {} ms after netd came up",
            show(lease.address),
            lease.prefix,
            show(lease.server),
            lease.router.map_or_else(|| "none".to_string(), show),
            lease.dns.iter().map(|d| show(*d)).collect::<Vec<_>>().join(" "),
            now.saturating_duration_since(self.began).as_millis(),
        );
    }

    /// Whether this machine's address question has just been settled — a
    /// lease held, or the bound passed with none — which is the moment netd
    /// has something to serve with.
    pub fn settle(&mut self, now: Instant) -> bool {
        if self.settled {
            return false;
        }
        if self.leased() {
            self.settled = true;
            return true;
        }
        if now.saturating_duration_since(self.began) >= LEASE_BOUND {
            crate::say!(
                "netd: DHCP: no lease as {} in {} s; this machine has no address and every \
                 connect through it is refused",
                HOSTNAME,
                now.saturating_duration_since(self.began).as_secs(),
            );
            self.settled = true;
            return true;
        }
        false
    }

    /// When the report owes its settling, if it still does.
    pub fn settle_at(&self) -> Option<Instant> {
        (!self.settled).then_some(self.began + LEASE_BOUND)
    }
}

fn show(addr: [u8; 4]) -> String {
    std::net::Ipv4Addr::from(addr).to_string()
}

#[cfg(test)]
mod tests;
