//! ToyOS's DHCPv4 client (RFC 2131, RFC 2132) with long options (RFC 3396), rapid commit
//! (RFC 4039), an RFC 4361 client identifier a server may echo (RFC 6842), and conflict
//! detection before first use (RFC 5227), which `toyos-net-ip` runs and reports back. Pure: the
//! caller hands in the time, the draws and a server's UDP payloads, and carries out what each
//! call returns — at most one transmission, a configuration change, a request about the address.
//! Nothing here reads a clock, draws randomness or does I/O.
//!
//! **Draws in a fixed order.** A call that starts an exchange draws its xid first; one that arms
//! a jittered wait then draws the jitter; nothing else draws. The xid is the draw itself: the
//! only thing keeping an off-link host from forging replies to this client.
//!
//! **Modern only.** A BOOTP reply, an unauthenticated FORCERENEW and a lease without a subnet
//! mask are refused, counted and named in a [`Refusal`]; DHCPINFORM and DHCPRELEASE are never sent.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

extern crate alloc;

mod message;

use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::Instant;

use message::{Build, Kind, Reply, ReplyKind};

toyos_net_wire::counters! {
    Truncated = "dhcp.truncated", logged;
    NotReply = "dhcp.not-reply", logged;
    HardwareType = "dhcp.hardware-type", logged;
    ChaddrMismatch = "dhcp.chaddr-mismatch";
    BootpReply = "dhcp.bootp-reply", logged;
    OptionTruncated = "dhcp.option-truncated", logged;
    NoEnd = "dhcp.no-end";
    OverloadInvalid = "dhcp.overload-invalid", logged;
    OptionLength = "dhcp.option-length", logged;
    MessageTypeMissing = "dhcp.message-type-missing", logged;
    WrongDirection = "dhcp.wrong-direction", logged;
    Forcerenew = "dhcp.forcerenew", logged;
    MessageTypeUnsupported = "dhcp.message-type-unsupported", logged;
    XidMismatch = "dhcp.xid-mismatch";
    ClientIdMismatch = "dhcp.client-id-mismatch", logged;
    NoServerId = "dhcp.no-server-id", logged;
    ServerIdInvalid = "dhcp.server-id-invalid", logged;
    UnexpectedOffer = "dhcp.unexpected-offer";
    UnexpectedAck = "dhcp.unexpected-ack";
    UnexpectedNak = "dhcp.unexpected-nak";
    YiaddrInvalid = "dhcp.yiaddr-invalid", logged;
    NoLeaseTime = "dhcp.no-lease-time", logged;
    LeaseZero = "dhcp.lease-zero", logged;
    NoSubnetMask = "dhcp.no-subnet-mask", logged;
    MaskInvalid = "dhcp.mask-invalid", logged;
    RouterInvalid = "dhcp.router-invalid", logged;
    DnsInvalid = "dhcp.dns-invalid", logged;
    DnsTruncated = "dhcp.dns-truncated";
    TimerOptionInvalid = "dhcp.timer-option-invalid", logged;
    AckWrongServer = "dhcp.ack-wrong-server", logged;
    AckAddressChanged = "dhcp.ack-address-changed", logged;
    NakWrongServer = "dhcp.nak-wrong-server", logged;
    RequestTimeout = "dhcp.request-timeout", logged;
    LeaseExpired = "dhcp.lease-expired", logged;
    Nak = "dhcp.nak", logged;
    RebootUnanswered = "dhcp.reboot-unanswered";
    Declined = "dhcp.declined", logged;
    NakBackoff = "dhcp.nak-backoff";
    TxDiscover = "dhcp.tx.discover";
    TxRequest = "dhcp.tx.request";
    TxDecline = "dhcp.tx.decline";
    RxOffer = "dhcp.rx.offer";
    RxAck = "dhcp.rx.ack";
    RxNak = "dhcp.rx.nak";
    EventOverflow = "dhcp.event-overflow";
}

pub mod limits {
    use core::time::Duration;

    pub const FIRST_WAIT: Duration = Duration::from_secs(4);
    pub const MAX_WAIT: Duration = Duration::from_secs(64);
    pub const REQUEST_TRANSMISSIONS: u32 = 4;
    pub const RENEW_FLOOR: Duration = Duration::from_secs(60);
    pub const DECLINE_BACKOFF: Duration = Duration::from_secs(10);
    pub const MAX_DNS: usize = 3;
    /// The largest message the client accepts: a 1,500-byte datagram under either reading of
    /// RFC 2132 §9.10.
    pub const MAX_MESSAGE: u16 = 1_472;
    /// RFC 1542 §2.1's minimum; every message the client sends is padded to it.
    pub const MIN_SENT: usize = 300;
    /// Refusals held for the shell until it drains them.
    pub const EVENTS: usize = 1_024;
}

/// A host name the client may send: one DNS label (RFC 1035 §2.3.1, RFC 1123 §2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostName(Vec<u8>);

impl HostName {
    pub fn new(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        let label = (1..=63).contains(&bytes.len())
            && bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            && bytes.first() != Some(&b'-')
            && bytes.last() != Some(&b'-');
        label.then(|| Self(bytes.to_vec()))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timers {
    pub t1: Instant,
    pub t2: Instant,
    pub expiry: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub server: Ipv4Addr,
    /// The first transmission of the request the ACK answered (RFC 2131 §4.4.1).
    pub base: Instant,
    /// `None` for an infinite lease.
    pub timers: Option<Timers>,
}

impl Lease {
    fn expired(&self, now: Instant) -> bool {
        self.timers.is_some_and(|t| now >= t.expiry)
    }
}

/// A message to send: UDP 68 → 67, built by the stack around this payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transmission {
    /// 0.0.0.0, or the lease's address when renewing or rebinding.
    pub source: Ipv4Addr,
    /// The limited broadcast, or the lease's server identifier when renewing.
    pub destination: Ipv4Addr,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Expired,
    Refused,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Config {
    /// A new lease: address, prefix, route and resolvers together.
    Configured(Lease),
    /// The same parameters, new times.
    Extended(Lease),
    /// The same address with a changed prefix, router or resolvers.
    Reconfigured(Lease),
    /// Address, route and resolvers go.
    Deconfigured(Reason),
}

/// What the shell asks `toyos-net-ip` for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressRequest {
    /// Add as tentative: conflict detection runs, then reports back (RFC 5227 §2.1).
    Probe { address: Ipv4Addr, prefix_len: u8 },
    /// Remove the tentative address.
    Cancel(Ipv4Addr),
}

/// What one call asks the shell to do: at most one of each.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub transmit: Option<Transmission>,
    pub config: Option<Config>,
    pub address: Option<AddressRequest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Selecting,
    Requesting,
    Probing,
    Bound,
    Renewing,
    Rebinding,
    Rebooting,
    BackingOff,
}

/// Who a logged refusal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// A message's IPv4 source.
    From(Ipv4Addr),
    /// The server a request went unanswered by.
    Server(Ipv4Addr),
    /// The address a lease held.
    Lease(Ipv4Addr),
    /// The host an address was found in use by.
    Conflict { address: Ipv4Addr, mac: MacAddr },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rule: Counter,
    pub peer: Peer,
}

#[derive(Debug)]
struct Selecting {
    xid: u32,
    /// The exchange's first DISCOVER: a rapid-commit lease runs from it.
    start: Instant,
    /// The last DISCOVER's, which the REQUEST repeats (RFC 2131 §3.1 (3)).
    secs: u16,
    step: u32,
    deadline: Instant,
}

#[derive(Debug)]
struct Requesting {
    xid: u32,
    server: Ipv4Addr,
    address: Ipv4Addr,
    secs: u16,
    first: Instant,
    step: u32,
    deadline: Instant,
}

/// RENEWING or REBINDING: each transmission is an exchange of its own.
#[derive(Debug)]
struct Renewal {
    lease: Lease,
    xid: u32,
    /// The first transmission of this lease period's renewal, which `secs` counts from.
    started: Instant,
    /// This transmission, which an ACK to it makes the lease's base.
    sent: Instant,
    deadline: Instant,
}

#[derive(Debug)]
struct Rebooting {
    lease: Lease,
    xid: u32,
    start: Instant,
    step: u32,
    deadline: Instant,
}

#[derive(Debug)]
enum State {
    Selecting(Selecting),
    Requesting(Requesting),
    /// The acknowledged lease, its address under conflict detection.
    Probing(Lease),
    Bound(Lease),
    Renewing(Renewal),
    Rebinding(Renewal),
    Rebooting(Rebooting),
    BackingOff(Instant),
}

pub struct Client {
    mac: IndividualMac,
    client_id: [u8; 15],
    host_name: Option<HostName>,
    state: State,
    /// Consecutive NAKs since the last BOUND (§D11).
    naks: u32,
    counters: Counters,
    refusals: Vec<Refusal>,
}

/// Not 0.0.0.0/8, 127.0.0.0/8, 169.254.0.0/16, 224.0.0.0/4 or 240.0.0.0/4: one host (§D4.1).
fn host(addr: Ipv4Addr) -> bool {
    let [a, b, ..] = addr.octets();
    !(a == 0 || a == 127 || (a, b) == (169, 254) || a >= 224)
}

fn mask(len: u8) -> u32 {
    u32::MAX.checked_shl(u32::from(32u8.saturating_sub(len))).unwrap_or(0)
}

/// A contiguous mask of 1 to 31 bits (H-14).
fn prefix_len(mask_addr: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(mask_addr);
    let len = u8::try_from(bits.leading_ones()).ok()?;
    ((1..=31).contains(&len) && mask(len) == bits).then_some(len)
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, len: u8) -> bool {
    (u32::from(a) ^ u32::from(b)) & mask(len) == 0
}

/// The directed broadcast of a prefix of /30 or shorter.
fn broadcast(addr: Ipv4Addr, len: u8) -> Option<Ipv4Addr> {
    (len <= 30).then(|| Ipv4Addr::from(u32::from(addr) | !mask(len)))
}

fn secs(since: Duration) -> u16 {
    u16::try_from(since.as_secs()).unwrap_or(u16::MAX)
}

/// 4, 8, 16, 32, then 64 s (RFC 2131 §4.1).
fn backoff(step: u32) -> Duration {
    2u32.checked_pow(step).and_then(|m| limits::FIRST_WAIT.checked_mul(m)).map_or(limits::MAX_WAIT, |d| d.min(limits::MAX_WAIT))
}

/// A wait moved by (draw mod 2,001) − 1,000 ms (§D6.3 (1)).
fn jittered(wait: Duration, draw: u32) -> Duration {
    let jitter = u64::from(draw.checked_rem(2_001).unwrap_or(0));
    let ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX).saturating_add(jitter).saturating_sub(1_000);
    Duration::from_millis(ms)
}

/// Half the time left until `until`, and never less than the floor (RFC 2131 §4.4.5).
fn halfway(now: Instant, until: Instant) -> Instant {
    let half = Duration::from_nanos(until.since(now).as_nanos().checked_div(2).and_then(|n| u64::try_from(n).ok()).unwrap_or(0));
    now.after(half.max(limits::RENEW_FLOOR)).min(until)
}

fn ms(seconds: u32) -> Duration {
    Duration::from_millis(u64::from(seconds).saturating_mul(1_000))
}

impl Client {
    /// Begins the first exchange: a DISCOVER now (DIV-D1: no initial delay).
    pub fn start(now: Instant, mac: IndividualMac, host_name: Option<HostName>, mut draw: impl FnMut() -> u32) -> (Self, Output) {
        let [m0, m1, m2, m3, m4, m5] = mac.get().0;
        let client_id = [255, m2, m3, m4, m5, 0, 3, 0, 1, m0, m1, m2, m3, m4, m5];
        let mut client = Self {
            mac,
            client_id,
            host_name,
            state: State::BackingOff(now),
            naks: 0,
            counters: Counters::default(),
            refusals: Vec::new(),
        };
        let out = client.begin(now, &mut draw);
        (client, out)
    }

    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Refusals to log since the last call.
    pub fn drain_refusals(&mut self) -> alloc::vec::Drain<'_, Refusal> {
        self.refusals.drain(..)
    }

    pub fn phase(&self) -> Phase {
        match self.state {
            State::Selecting(_) => Phase::Selecting,
            State::Requesting(_) => Phase::Requesting,
            State::Probing(_) => Phase::Probing,
            State::Bound(_) => Phase::Bound,
            State::Renewing(_) => Phase::Renewing,
            State::Rebinding(_) => Phase::Rebinding,
            State::Rebooting(_) => Phase::Rebooting,
            State::BackingOff(_) => Phase::BackingOff,
        }
    }

    /// The transaction a reply must carry; none while bound, probing or backing off.
    pub fn xid(&self) -> Option<u32> {
        match &self.state {
            State::Selecting(s) => Some(s.xid),
            State::Requesting(r) => Some(r.xid),
            State::Renewing(r) | State::Rebinding(r) => Some(r.xid),
            State::Rebooting(r) => Some(r.xid),
            State::Probing(_) | State::Bound(_) | State::BackingOff(_) => None,
        }
    }

    /// The lease in use: configured, or being renewed or verified.
    pub fn lease(&self) -> Option<&Lease> {
        match &self.state {
            State::Bound(lease) => Some(lease),
            State::Renewing(r) | State::Rebinding(r) => Some(&r.lease),
            State::Rebooting(r) => Some(&r.lease),
            State::Selecting(_) | State::Requesting(_) | State::Probing(_) | State::BackingOff(_) => None,
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        match &self.state {
            State::Selecting(s) => Some(s.deadline),
            State::Requesting(r) => Some(r.deadline),
            State::Probing(_) => None,
            State::Bound(lease) => lease.timers.map(|t| t.t1),
            State::Renewing(r) | State::Rebinding(r) => Some(r.deadline),
            State::Rebooting(r) => Some(r.lease.timers.map_or(r.deadline, |t| r.deadline.min(t.expiry))),
            State::BackingOff(until) => Some(*until),
        }
    }

    fn refuse(&mut self, rule: Counter, peer: Peer) {
        self.counters.add(rule, 1);
        if !rule.logged() {
            return;
        }
        if self.refusals.len() >= limits::EVENTS {
            self.counters.add(Counter::EventOverflow, 1);
        } else {
            self.refusals.push(Refusal { rule, peer });
        }
    }

    fn message(&self, kind: Kind, xid: u32, secs: u16, ciaddr: Ipv4Addr, server: Option<Ipv4Addr>, requested: Option<Ipv4Addr>) -> Vec<u8> {
        Build {
            kind,
            xid,
            secs,
            ciaddr,
            server,
            requested,
            mac: self.mac.get(),
            client_id: &self.client_id,
            host_name: self.host_name.as_ref(),
        }
        .bytes()
    }

    fn broadcast(&mut self, payload: Vec<u8>, counter: Counter) -> Transmission {
        self.counters.add(counter, 1);
        Transmission { source: Ipv4Addr::UNSPECIFIED, destination: Ipv4Addr::BROADCAST, payload }
    }

    /// A new exchange: an xid, a DISCOVER now, and its first jittered wait (INIT → SELECTING).
    fn begin(&mut self, now: Instant, draw: &mut impl FnMut() -> u32) -> Output {
        let xid = draw();
        let payload = self.message(Kind::Discover, xid, 0, Ipv4Addr::UNSPECIFIED, None, None);
        let transmit = self.broadcast(payload, Counter::TxDiscover);
        let deadline = now.after(jittered(backoff(0), draw()));
        self.state = State::Selecting(Selecting { xid, start: now, secs: 0, step: 0, deadline });
        Output { transmit: Some(transmit), ..Output::default() }
    }

    /// A DECLINE of `address` to `server`, then 10 s of BACKING-OFF (RFC 2131 §3.1 (5)).
    fn decline(&mut self, now: Instant, address: Ipv4Addr, server: Ipv4Addr, mac: MacAddr, draw: &mut impl FnMut() -> u32) -> Transmission {
        let xid = draw();
        let payload = self.message(Kind::Decline, xid, 0, Ipv4Addr::UNSPECIFIED, Some(server), Some(address));
        self.refuse(Counter::Declined, Peer::Conflict { address, mac });
        self.state = State::BackingOff(now.after(limits::DECLINE_BACKOFF));
        self.broadcast(payload, Counter::TxDecline)
    }

    /// A RENEWING (unicast to the server) or REBINDING (broadcast) request with a fresh xid.
    fn renew(&mut self, now: Instant, lease: Lease, timers: Timers, started: Instant, rebind: bool, draw: &mut impl FnMut() -> u32) -> Output {
        let xid = draw();
        let payload = self.message(Kind::Request, xid, secs(now.since(started)), lease.address, None, None);
        self.counters.add(Counter::TxRequest, 1);
        let (destination, deadline) = if rebind {
            (Ipv4Addr::BROADCAST, halfway(now, timers.expiry))
        } else {
            (lease.server, halfway(now, timers.t2))
        };
        let transmit = Transmission { source: lease.address, destination, payload };
        let renewal = Renewal { lease, xid, started, sent: now, deadline };
        self.state = if rebind { State::Rebinding(renewal) } else { State::Renewing(renewal) };
        Output { transmit: Some(transmit), ..Output::default() }
    }

    /// The lease ended: deconfigured, and a new exchange in the same call (RFC 2131 §4.4.5).
    fn expire(&mut self, now: Instant, address: Ipv4Addr, draw: &mut impl FnMut() -> u32) -> Output {
        self.refuse(Counter::LeaseExpired, Peer::Lease(address));
        Output { config: Some(Config::Deconfigured(Reason::Expired)), ..self.begin(now, draw) }
    }

    /// A lease in use whose deadlines may be due: only the latest due event runs (§D5.2).
    fn run_lease(&mut self, now: Instant, lease: Lease, started: Option<Instant>, draw: &mut impl FnMut() -> u32) -> Output {
        let Some(timers) = lease.timers else {
            self.state = State::Bound(lease);
            return Output::default();
        };
        if now >= timers.expiry {
            return self.expire(now, lease.address, draw);
        }
        if now >= timers.t2 {
            return self.renew(now, lease, timers, started.unwrap_or(now), true, draw);
        }
        if now >= timers.t1 {
            return self.renew(now, lease, timers, started.unwrap_or(now), false, draw);
        }
        self.state = State::Bound(lease);
        Output::default()
    }

    /// Enters BOUND with `lease`, or at once RENEWING or REBINDING when those are due.
    fn bind(&mut self, now: Instant, lease: Lease, draw: &mut impl FnMut() -> u32) -> Output {
        self.naks = 0;
        self.run_lease(now, lease, None, draw)
    }

    /// A NAK the state accepted (§D11): the n-th in a row delays the new exchange by 0, 4, 8, 16,
    /// 32, then 64 s, each delayed one jittered.
    fn nak(&mut self, now: Instant, draw: &mut impl FnMut() -> u32) -> Output {
        self.naks = self.naks.saturating_add(1);
        match self.naks.checked_sub(2) {
            None => self.begin(now, draw),
            Some(step) => {
                let wait = jittered(backoff(step), draw());
                self.counters.add(Counter::NakBackoff, 1);
                self.state = State::BackingOff(now.after(wait));
                Output::default()
            }
        }
    }

    pub fn timer(&mut self, now: Instant, mut draw: impl FnMut() -> u32) -> Output {
        if self.next_deadline().is_none_or(|d| now < d) {
            return Output::default();
        }
        let state = core::mem::replace(&mut self.state, State::BackingOff(now));
        match state {
            State::Selecting(mut s) => {
                s.secs = secs(now.since(s.start));
                s.step = s.step.saturating_add(1);
                let payload = self.message(Kind::Discover, s.xid, s.secs, Ipv4Addr::UNSPECIFIED, None, None);
                let transmit = self.broadcast(payload, Counter::TxDiscover);
                s.deadline = now.after(jittered(backoff(s.step), draw()));
                self.state = State::Selecting(s);
                Output { transmit: Some(transmit), ..Output::default() }
            }
            State::Requesting(r) if r.step.saturating_add(1) >= limits::REQUEST_TRANSMISSIONS => {
                self.refuse(Counter::RequestTimeout, Peer::Server(r.server));
                self.begin(now, &mut draw)
            }
            State::Requesting(mut r) => {
                r.step = r.step.saturating_add(1);
                let payload = self.message(Kind::Request, r.xid, r.secs, Ipv4Addr::UNSPECIFIED, Some(r.server), Some(r.address));
                let transmit = self.broadcast(payload, Counter::TxRequest);
                r.deadline = now.after(jittered(backoff(r.step), draw()));
                self.state = State::Requesting(r);
                Output { transmit: Some(transmit), ..Output::default() }
            }
            State::Bound(lease) => self.run_lease(now, lease, None, &mut draw),
            State::Renewing(r) | State::Rebinding(r) => self.run_lease(now, r.lease, Some(r.started), &mut draw),
            State::Rebooting(r) if r.lease.expired(now) => self.expire(now, r.lease.address, &mut draw),
            State::Rebooting(r) if r.step.saturating_add(1) >= limits::REQUEST_TRANSMISSIONS => {
                self.counters.add(Counter::RebootUnanswered, 1);
                self.run_lease(now, r.lease, None, &mut draw)
            }
            State::Rebooting(mut r) => {
                r.step = r.step.saturating_add(1);
                let transmit = self.reboot_request(now, &r);
                r.deadline = now.after(jittered(backoff(r.step), draw()));
                self.state = State::Rebooting(r);
                Output { transmit: Some(transmit), ..Output::default() }
            }
            State::BackingOff(_) => self.begin(now, &mut draw),
            State::Probing(lease) => {
                self.state = State::Probing(lease);
                Output::default()
            }
        }
    }

    /// INIT-REBOOT's REQUEST: the held address in option 50, no server identifier (RFC 2131 §4.4.2).
    fn reboot_request(&mut self, now: Instant, r: &Rebooting) -> Transmission {
        let payload = self.message(Kind::Request, r.xid, secs(now.since(r.start)), Ipv4Addr::UNSPECIFIED, None, Some(r.lease.address));
        self.broadcast(payload, Counter::TxRequest)
    }

    /// The link came up (§D9): without a lease the exchange starts over; with one, INIT-REBOOT
    /// verifies it while it stays in use, and [ip] announces it rather than probing again.
    pub fn link_up(&mut self, now: Instant, mut draw: impl FnMut() -> u32) -> Output {
        let state = core::mem::replace(&mut self.state, State::BackingOff(now));
        let lease = match state {
            State::Bound(lease) => lease,
            State::Renewing(r) | State::Rebinding(r) => r.lease,
            State::Rebooting(r) => r.lease,
            State::Probing(lease) => {
                return Output { address: Some(AddressRequest::Cancel(lease.address)), ..self.begin(now, &mut draw) };
            }
            State::Selecting(_) | State::Requesting(_) | State::BackingOff(_) => return self.begin(now, &mut draw),
        };
        if lease.expired(now) {
            return self.expire(now, lease.address, &mut draw);
        }
        let xid = draw();
        let r = Rebooting { lease, xid, start: now, step: 0, deadline: now };
        let transmit = self.reboot_request(now, &r);
        let deadline = now.after(jittered(backoff(0), draw()));
        self.state = State::Rebooting(Rebooting { deadline, ..r });
        Output { transmit: Some(transmit), ..Output::default() }
    }

    /// [ip] finished conflict detection: the address is in use from here (§D8 (2)).
    pub fn verified(&mut self, now: Instant, mut draw: impl FnMut() -> u32) -> Output {
        let lease = match core::mem::replace(&mut self.state, State::BackingOff(now)) {
            State::Probing(lease) => lease,
            other => {
                self.state = other;
                return Output::default();
            }
        };
        if lease.expired(now) {
            self.refuse(Counter::LeaseExpired, Peer::Lease(lease.address));
            return self.begin(now, &mut draw);
        }
        let configured = Config::Configured(lease.clone());
        Output { config: Some(configured), ..self.bind(now, lease, &mut draw) }
    }

    /// Another host holds the address: while probing, a DECLINE; while in use, it was lost after
    /// [ip]'s one defence, so it is deconfigured and declined (§D8 (3), (5)).
    pub fn conflict(&mut self, now: Instant, mac: MacAddr, mut draw: impl FnMut() -> u32) -> Output {
        let state = core::mem::replace(&mut self.state, State::BackingOff(now));
        let (lease, in_use) = match state {
            State::Probing(lease) => (lease, false),
            State::Bound(lease) => (lease, true),
            State::Renewing(r) | State::Rebinding(r) => (r.lease, true),
            State::Rebooting(r) => (r.lease, true),
            other => {
                self.state = other;
                return Output::default();
            }
        };
        let transmit = self.decline(now, lease.address, lease.server, mac, &mut draw);
        let config = in_use.then_some(Config::Deconfigured(Reason::Conflict));
        Output { transmit: Some(transmit), config, address: None }
    }

    /// The link went down while [ip] probed: the exchange is abandoned (§D8 (6)).
    pub fn not_verified(&mut self, now: Instant, mut draw: impl FnMut() -> u32) -> Output {
        match self.state {
            State::Probing(_) => self.begin(now, &mut draw),
            _ => Output::default(),
        }
    }

    /// A server's UDP payload and the IPv4 source it came from (§D3).
    pub fn receive(&mut self, now: Instant, payload: &[u8], from: Ipv4Addr, mut draw: impl FnMut() -> u32) -> Output {
        let peer = Peer::From(from);
        let reply = match Reply::parse(payload, self.mac.get()) {
            Ok(reply) => reply,
            Err(rule) => {
                self.refuse(rule, peer);
                return Output::default();
            }
        };
        self.counters.add(Counter::NoEnd, reply.no_end);
        self.counters.add(
            match reply.kind {
                ReplyKind::Offer => Counter::RxOffer,
                ReplyKind::Ack => Counter::RxAck,
                ReplyKind::Nak => Counter::RxNak,
            },
            1,
        );
        if self.xid() != Some(reply.xid) {
            self.refuse(Counter::XidMismatch, peer);
            return Output::default();
        }
        if reply.client_id.as_deref().is_some_and(|id| id != self.client_id) {
            self.refuse(Counter::ClientIdMismatch, peer);
            return Output::default();
        }
        let server = match reply.server {
            None => return self.refused(Counter::NoServerId, peer),
            Some(server) if !host(server) => return self.refused(Counter::ServerIdInvalid, peer),
            Some(server) => server,
        };
        let unexpected = match (&self.state, reply.kind) {
            (State::Selecting(_), ReplyKind::Ack) if !reply.rapid => Some(Counter::UnexpectedAck),
            (State::Selecting(_), ReplyKind::Nak) => Some(Counter::UnexpectedNak),
            (State::Selecting(_), _) => None,
            (_, ReplyKind::Offer) => Some(Counter::UnexpectedOffer),
            _ => None,
        };
        if let Some(rule) = unexpected {
            return self.refused(rule, peer);
        }
        match reply.kind {
            ReplyKind::Nak => self.receive_nak(now, server, peer, &mut draw),
            ReplyKind::Offer | ReplyKind::Ack => {
                let Some(offered) = self.acceptable(&reply, server, peer) else { return Output::default() };
                self.accept(now, reply.kind, offered, peer, &mut draw)
            }
        }
    }

    fn refused(&mut self, rule: Counter, peer: Peer) -> Output {
        self.refuse(rule, peer);
        Output::default()
    }

    /// §D4.1: the address, the lease time and the mask an OFFER or ACK must carry.
    fn acceptable(&mut self, reply: &Reply, server: Ipv4Addr, peer: Peer) -> Option<Offered> {
        let yiaddr = reply.yiaddr;
        let refusal = if !host(yiaddr) {
            Some(Counter::YiaddrInvalid)
        } else {
            match (reply.lease, reply.mask) {
                (None, _) => Some(Counter::NoLeaseTime),
                (Some(0), _) => Some(Counter::LeaseZero),
                (_, None) => Some(Counter::NoSubnetMask),
                (Some(_), Some(m)) => match prefix_len(m) {
                    None => Some(Counter::MaskInvalid),
                    Some(len) if len <= 30 && (yiaddr == Ipv4Addr::from(u32::from(yiaddr) & mask(len)) || Some(yiaddr) == broadcast(yiaddr, len)) => {
                        Some(Counter::YiaddrInvalid)
                    }
                    Some(_) => None,
                },
            }
        };
        if let Some(rule) = refusal {
            self.refuse(rule, peer);
            return None;
        }
        Some(Offered {
            address: yiaddr,
            prefix_len: reply.mask.and_then(prefix_len).unwrap_or(32),
            server,
            lease: reply.lease.unwrap_or(0),
            routers: reply.routers.clone(),
            dns: reply.dns.clone(),
            t1: reply.t1,
            t2: reply.t2,
        })
    }

    fn receive_nak(&mut self, now: Instant, server: Ipv4Addr, peer: Peer, draw: &mut impl FnMut() -> u32) -> Output {
        let wrong = match &self.state {
            State::Requesting(r) => r.server != server,
            State::Renewing(r) => r.lease.server != server,
            State::Rebinding(_) | State::Rebooting(_) => false,
            State::Selecting(_) | State::Probing(_) | State::Bound(_) | State::BackingOff(_) => return Output::default(),
        };
        if wrong {
            return self.refused(Counter::NakWrongServer, peer);
        }
        if matches!(self.state, State::Requesting(_)) {
            return self.nak(now, draw);
        }
        self.refuse(Counter::Nak, peer);
        Output { config: Some(Config::Deconfigured(Reason::Refused)), ..self.nak(now, draw) }
    }

    /// An acceptable OFFER or ACK for the state the client is in (§D5.2): an ACK must name the
    /// server asked while REQUESTING or RENEWING, and the address asked for or held.
    fn accept(&mut self, now: Instant, kind: ReplyKind, offered: Offered, peer: Peer, draw: &mut impl FnMut() -> u32) -> Output {
        let state = core::mem::replace(&mut self.state, State::BackingOff(now));
        match (state, kind) {
            (State::Selecting(s), ReplyKind::Offer) => {
                let payload = self.message(Kind::Request, s.xid, s.secs, Ipv4Addr::UNSPECIFIED, Some(offered.server), Some(offered.address));
                let transmit = self.broadcast(payload, Counter::TxRequest);
                let deadline = now.after(jittered(backoff(0), draw()));
                self.state = State::Requesting(Requesting {
                    xid: s.xid,
                    server: offered.server,
                    address: offered.address,
                    secs: s.secs,
                    first: now,
                    step: 0,
                    deadline,
                });
                Output { transmit: Some(transmit), ..Output::default() }
            }
            (State::Selecting(s), _) => {
                let lease = self.lease_of(offered, s.start, peer);
                self.probe(lease)
            }
            (State::Requesting(r), _) if offered.server != r.server => self.keep(State::Requesting(r), Counter::AckWrongServer, peer),
            (State::Requesting(r), _) if offered.address != r.address => self.keep(State::Requesting(r), Counter::AckAddressChanged, peer),
            (State::Requesting(r), _) => {
                let lease = self.lease_of(offered, r.first, peer);
                self.probe(lease)
            }
            (State::Renewing(r), _) if offered.server != r.lease.server => self.keep(State::Renewing(r), Counter::AckWrongServer, peer),
            (State::Renewing(r), _) if offered.address != r.lease.address => self.keep(State::Renewing(r), Counter::AckAddressChanged, peer),
            (State::Rebinding(r), _) if offered.address != r.lease.address => self.keep(State::Rebinding(r), Counter::AckAddressChanged, peer),
            (State::Renewing(r) | State::Rebinding(r), _) => {
                let lease = self.lease_of(offered, r.sent, peer);
                self.renewed(now, &r.lease, lease, draw)
            }
            (State::Rebooting(r), _) if offered.address != r.lease.address => self.keep(State::Rebooting(r), Counter::AckAddressChanged, peer),
            (State::Rebooting(r), _) => {
                let lease = self.lease_of(offered, r.start, peer);
                self.renewed(now, &r.lease, lease, draw)
            }
            (state @ (State::Probing(_) | State::Bound(_) | State::BackingOff(_)), _) => {
                self.state = state;
                Output::default()
            }
        }
    }

    fn keep(&mut self, state: State, rule: Counter, peer: Peer) -> Output {
        self.state = state;
        self.refused(rule, peer)
    }

    /// An acknowledged new address: [ip] probes it before it is used (RFC 5227 §2.1).
    fn probe(&mut self, lease: Lease) -> Output {
        let address = Some(AddressRequest::Probe { address: lease.address, prefix_len: lease.prefix_len });
        self.state = State::Probing(lease);
        Output { address, ..Output::default() }
    }

    /// The held address acknowledged again: extended, or reconfigured when its prefix, router or
    /// resolvers changed.
    fn renewed(&mut self, now: Instant, old: &Lease, lease: Lease, draw: &mut impl FnMut() -> u32) -> Output {
        let same = old.prefix_len == lease.prefix_len && old.router == lease.router && old.dns == lease.dns;
        let config = if same { Config::Extended(lease.clone()) } else { Config::Reconfigured(lease.clone()) };
        Output { config: Some(config), ..self.bind(now, lease, draw) }
    }

    /// §D4.2 and §D7: the first usable router, at most three resolvers, and T1 and T2, the
    /// lease running from `base`.
    fn lease_of(&mut self, offered: Offered, base: Instant, peer: Peer) -> Lease {
        let (address, len) = (offered.address, offered.prefix_len);
        let mut router = None;
        for candidate in offered.routers {
            if host(candidate) && same_subnet(candidate, address, len) && candidate != address {
                router = Some(candidate);
                break;
            }
            self.refuse(Counter::RouterInvalid, peer);
        }
        let mut dns = Vec::new();
        for server in offered.dns {
            if !host(server) || Some(server) == broadcast(address, len) {
                self.refuse(Counter::DnsInvalid, peer);
            } else if dns.len() < limits::MAX_DNS {
                dns.push(server);
            } else {
                self.counters.add(Counter::DnsTruncated, 1);
            }
        }
        let timers = if offered.lease == u32::MAX {
            for _ in offered.t1.iter().chain(offered.t2.iter()) {
                self.refuse(Counter::TimerOptionInvalid, peer);
            }
            None
        } else {
            let lease = ms(offered.lease);
            let t2 = match offered.t2 {
                Some(t2) if t2 > 0 && t2 < offered.lease => ms(t2),
                given => {
                    if given.is_some() {
                        self.refuse(Counter::TimerOptionInvalid, peer);
                    }
                    lease.saturating_mul(7).checked_div(8).unwrap_or(lease)
                }
            };
            let t1 = match offered.t1 {
                Some(t1) if t1 > 0 && ms(t1) < t2 => ms(t1),
                given => {
                    if given.is_some() {
                        self.refuse(Counter::TimerOptionInvalid, peer);
                    }
                    lease.checked_div(2).unwrap_or(lease).min(t2)
                }
            };
            Some(Timers { t1: base.after(t1), t2: base.after(t2), expiry: base.after(lease) })
        };
        Lease { address, prefix_len: len, router, dns, server: offered.server, base, timers }
    }
}

/// What an acceptable OFFER or ACK holds, before §D4.2 degrades it into a lease.
struct Offered {
    address: Ipv4Addr,
    prefix_len: u8,
    server: Ipv4Addr,
    lease: u32,
    routers: Vec<Ipv4Addr>,
    dns: Vec<Ipv4Addr>,
    t1: Option<u32>,
    t2: Option<u32>,
}
