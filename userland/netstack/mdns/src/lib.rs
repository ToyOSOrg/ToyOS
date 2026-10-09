//! Multicast DNS (RFC 6762) for one machine's own name: `<host>.local` answers
//! with the address its lease gave it, so a machine on the same network finds
//! it by name with no configuration anywhere — macOS resolves `.local` names
//! this way natively.
//!
//! **One record and nothing else.** This responder owns exactly one `A`
//! record, `<host>.local`, and answers a question for that name of type `A` or
//! `ANY`; every other question is somebody else's and gets silence, which is
//! what RFC 6762 §6 asks of a responder that has no answer.
//!
//! **The name is claimed before it is used** (§8: "Probing (Section 8.1) and
//! Announcing (Section 8.3)"), on every link that comes after none
//! ([`Responder::on`]). A link is an address on a link that is up: the caller
//! says it has none while it holds no address and while its link is down, so
//! an address after none and a link's return are one event here.
//!
//! - §8.1: after a delay the caller draws, uniform in 0 to 250 ms, three
//!   probes 250 ms apart, each the question `ANY` for the name with the
//!   unicast-response bit and the proposed record in the Authority Section;
//!   250 ms after the third the name is held, and only then is the record
//!   announced (§8.3) or answered with. A new address under a held name is
//!   announced and not probed for (§8.4).
//! - §8.1: "if any conflicting Multicast DNS response is received, then the
//!   probing host MUST defer to the existing host": the name is lost
//!   ([`Event::Lost`]). A response conflicts when it carries, in any section, a
//!   record of this name of any type other than this record itself (§9:
//!   "resource records with identical rdata are never considered
//!   inconsistent"); one "received *before* the first probe packet is sent MUST
//!   be silently ignored".
//! - §8.2: another host's probe for the name heard under ours is compared
//!   with ours, class, then type, then data as unsigned bytes, "and the
//!   lexicographically later data wins": the earlier "defers to the winning
//!   host by waiting one second, and then begins probing for this record
//!   again".
//! - §9: a conflicting response to a held name means it "MUST immediately
//!   reset its conflicted unique record to probing state", and "the protocol
//!   used in the Probing phase will determine a winner and a loser". A held
//!   name is defended by answering: another host's probe for it is a question
//!   like any other, multicast as soon as 250 ms after the record's last
//!   multicast (§6) or unicast at once.
//!
//! **The caller reads before it asks what is owed.** A pass is
//! [`Responder::on`], then [`Responder::heard`] for every message that has
//! arrived, then [`Responder::owed`]: a conflicting response already received
//! when the last 250 ms end is heard under the probe and takes the name,
//! where the other order would claim and announce it first.
//!
//! **A lost name is not replaced, and is probed for again.** §9 recommends a
//! responder change its name and probe again; this one holds none and says so
//! to its owner once. The name was moved in by the owner, who also asks the
//! network to record it with the lease: a responder that picked another would
//! answer to a name nothing else on the machine knows, that no storage keeps
//! for the next boot (§9's third step), and that any host on the link could
//! move again by answering for it. It probes for the same name again
//! [`RETRY_MS`] after each loss, and at once on a link after none: a probing
//! no host answers claims and announces the name as at start-up, and one the
//! holder answers waits the interval again and says nothing. §9's loser "MUST
//! cease using the name", and a probe uses none: it is a question, and
//! nothing is announced or answered between a loss and a claim. §8.1 lets a
//! failed probe be tried again five seconds later and asks for no retry at
//! all.
//!
//! **What a peer can make this responder do.** Every byte read is a peer's,
//! from a source on this link (§11); none is trusted, and none is kept.
//!
//! - It keeps nothing a message brought: its state is its fixed fields, of
//!   which a conflict writes the claim, whether its loss was said and one
//!   time.
//! - One conflict costs at most one probing: three probes and two
//!   announcements.
//! - §8.1: "If fifteen conflicts occur within any ten-second period, then the
//!   host MUST wait at least five seconds before each successive additional
//!   probe attempt", and "a valid way to comply with this requirement is to
//!   always wait five seconds after any failed probe attempt before trying
//!   again." Here every probe attempt that would begin within ten seconds of
//!   a conflict waits five seconds first, whatever begins it: a conflict at a
//!   held name, a lost tiebreak or a link after none. A conflict with none in
//!   the ten seconds before it is probed on at once, as §9 asks. So a peer's
//!   messages buy at most one probing in five seconds, for as long as it
//!   sends them, and the name is held again when it stops.
//! - A lost name's probing begins at least [`RETRY_MS`] after the one
//!   before, whatever arrives: between two nothing is read, since no probe is
//!   out for a message to conflict with or tie; under a probe a conflicting
//!   response and a probe that wins the tiebreak each put the next probing
//!   the interval later, where §8.2's second would let forged probes buy one
//!   in five seconds. So a peer's messages buy at most three probes in the
//!   interval from a host that holds no name, no announcement and no word to
//!   its owner.
//! - A link after none is probed on at once, whoever holds the name, in place
//!   of the retry that was owed and never beside it. Under a lost name it
//!   costs what it costs under any, its three probes, and §8.1's five seconds
//!   first within ten of a conflict: one probing in five seconds while a
//!   holder answers each.
//! - Two packets from any host on the link take a held name, a response that
//!   takes it back to probing and a response under the probe, and one more in
//!   each interval keeps it: a host that answers every probe is what a holder
//!   of the name is. When it stops, the next probing claims the name. One
//!   that lets each retry claim the name and then takes it buys the owner two
//!   words in the interval, claimed and lost.
//!
//! Where an answer goes follows the question (§5.4, §6.7):
//!
//! - a query from port 5353 is a full responder's: the answer is multicast to
//!   the group, unless it set the unicast-response bit, which asks for it back
//!   at its own address;
//! - a query from any other port is a legacy resolver's (§6.7): the answer goes
//!   back to its address and port, carrying its ID and its question, with no
//!   cache-flush bit and a TTL of at most ten seconds.
//!
//! A message whose source is not on this link is ignored (§11), as is one
//! with an opcode or a response code other than zero (§18.3, §18.11), and the
//! record is multicast at most once a second (§6) — announcements included —
//! with a query inside that second answered when it ends ([`Responder`]).
//!
//! **Not implemented, and so not claimed.** The caller does not say how a
//! message was addressed, so two rules that turn on it are not applied. §6: a
//! unicast response is read only within two seconds of a question that asked
//! for one; here a response is read however it came, and a host that can send
//! one can multicast it too. §11: a message sent to the group is on the link
//! "regardless of source IP address"; here one from a source outside the
//! subnet is ignored however it was addressed, so a host of this name on
//! another subnet of the same link is never a conflict.
//!
//! A message is read off the wire by `toyos-dns`, the reader a resolver's
//! reply is read by.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O, no clock and no randomness
//! of its own.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use alloc::vec::Vec;
use core::cmp::Ordering;

use toyos_dns::{name_at, u16_at, Name};

/// The port every multicast DNS responder listens on (§3).
pub const PORT: u16 = 5353;

/// The IPv4 group a query is sent to (§3).
pub const GROUP: [u8; 4] = [224, 0, 0, 251];

/// [`GROUP`]'s Ethernet address: `01:00:5e` and the group's low 23 bits
/// (RFC 1112 §6.4), which is what a card's multicast filter is asked to pass.
pub const GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, GROUP[1] & 0x7f, GROUP[2], GROUP[3]];

/// The TTL of an address record in a multicast answer: §10's recommendation
/// for a record naming a host.
pub const TTL: u32 = 120;

/// §6.7: an answer to a legacy resolver carries a TTL of at most ten seconds.
pub const LEGACY_TTL: u32 = 10;

/// The domain every name here is under (§3).
const LOCAL: &[u8] = b"local";

const TYPE_A: u16 = 1;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
/// The top bit of a question's class: the asker wants the answer unicast (§5.4).
const UNICAST_RESPONSE: u16 = 0x8000;
/// The top bit of an answer's class: every cached record of this name is
/// replaced by this one (§10.2). Set on every multicast answer, because this
/// responder answers only with a name it holds.
const CACHE_FLUSH: u16 = 0x8000;
/// `QR` and `AA`: a response, authoritative (§18.2, §18.4).
const RESPONSE_FLAGS: u16 = 0x8400;
/// §18.3, §18.11: the opcode and the response code of any message read here
/// are zero.
const OPCODE_MASK: u16 = 0x7800;
const RCODE_MASK: u16 = 0x000f;
const QR: u16 = 0x8000;

/// A message's header (RFC 1035 §4.1.1): its sections begin behind it.
const HEADER: usize = 12;

/// The longest label (RFC 1035 §2.3.4).
const MAX_LABEL: usize = 63;

/// Where an answer goes.
#[derive(Debug, PartialEq, Eq)]
pub enum To {
    /// The group, on [`PORT`].
    Group,
    /// The asker's own address and port.
    Asker,
}

/// One answer, and where it goes.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub to: To,
    pub bytes: Vec<u8>,
}

/// Why a host name cannot be answered for.
#[derive(Debug, PartialEq, Eq)]
pub struct NotALabel;

/// This machine's name, checked once: one label of letters, digits and
/// hyphens (RFC 1123 §2.1), which is what it goes on the wire as.
#[derive(Clone, Copy, Debug)]
pub struct Host<'a>(&'a str);

impl<'a> Host<'a> {
    pub fn new(name: &'a str) -> Result<Self, NotALabel> {
        let fits = (1..=MAX_LABEL).contains(&name.len());
        let kept = name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if fits && kept && !name.starts_with('-') && !name.ends_with('-') {
            Ok(Self(name))
        } else {
            Err(NotALabel)
        }
    }

    fn put_name(&self, out: &mut Vec<u8>) {
        out.push(self.0.len() as u8);
        out.extend_from_slice(self.0.as_bytes());
        out.push(LOCAL.len() as u8);
        out.extend_from_slice(LOCAL);
        out.push(0);
    }

    /// Whether `name`, as a message spelled it, is this host's, whatever the
    /// case of its letters.
    fn is(&self, name: &Name) -> bool {
        let mut ours = Vec::with_capacity(self.0.len() + LOCAL.len() + 3);
        self.put_name(&mut ours);
        name.wire().eq_ignore_ascii_case(&ours)
    }
}

/// The unsolicited answer a responder sends for a name it has claimed (§8.3).
fn announcement(host: Host, addr: [u8; 4]) -> Vec<u8> {
    let mut out = header(0, RESPONSE_FLAGS, [0, 1, 0]);
    answer_record(&mut out, host, addr, CLASS_IN | CACHE_FLUSH, TTL);
    out
}

/// §8.1: "All probe queries SHOULD be done using the desired resource record
/// name and class (usually class 1, "Internet"), and query type "ANY" (255)",
/// "as "QU" questions with the unicast-response bit set". §8.2: "each host
/// populates the query message's Authority Section with the record or records
/// with the rdata that it would be proposing to use".
fn probe(host: Host, addr: [u8; 4]) -> Vec<u8> {
    let mut out = header(0, 0, [1, 0, 1]);
    host.put_name(&mut out);
    out.extend_from_slice(&TYPE_ANY.to_be_bytes());
    out.extend_from_slice(&(CLASS_IN | UNICAST_RESPONSE).to_be_bytes());
    answer_record(&mut out, host, addr, CLASS_IN, TTL);
    out
}

/// Where a message came from: the source address and port of its packet.
#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub addr: [u8; 4],
    pub port: u16,
}

/// This machine on its link: the address its lease gave it and the prefix
/// length of the subnet that address is on.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub addr: [u8; 4],
    pub prefix: u8,
}

impl Link {
    /// Whether a message from `addr` is not from off this link, which is what
    /// §11 refuses: in this subnet, or link-local (RFC 3927). A loopback
    /// source is off every link (RFC 1122 §3.2.1.3: a host silently discards
    /// a datagram carrying one), so it is refused wherever it arrived.
    fn holds(&self, addr: [u8; 4]) -> bool {
        let mask = u32::MAX.checked_shl(32 - u32::from(self.prefix.min(32))).unwrap_or(0);
        let (ours, theirs) = (u32::from_be_bytes(self.addr), u32::from_be_bytes(addr));
        ours & mask == theirs & mask || addr[..2] == [169, 254]
    }
}

/// What became of the name, for its owner's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Probing ended with no host answering: the name is this host's, and is
    /// announced and answered with from here on.
    Claimed,
    /// §8.1: another host answered for the name under the probe. It is not
    /// this host's, and nothing is announced or answered until it is
    /// [`Event::Claimed`]. Said once: a later probing that host answers too
    /// says nothing.
    Lost,
}

/// §8.1: "the host should first wait for a short random delay time, uniformly
/// distributed in the range 0-250 ms."
const PROBE_DELAY_MS: u32 = 250;

/// §8.1: "250 ms after the first query, the host should send a second; then,
/// 250 ms after that, a third. If, by 250 ms after the third probe, no
/// conflicting Multicast DNS responses have been received, the host may move
/// to the next step, announcing."
const PROBE_EVERY_MS: u64 = 250;
const PROBES: u8 = 3;

/// §8.2: the loser of a simultaneous probe "defers to the winning host by
/// waiting one second, and then begins probing for this record again."
const DEFER_MS: u64 = 1_000;

/// §8.1: conflicts "within any ten-second period", and "at least five seconds
/// before each successive additional probe attempt."
const CONFLICT_WINDOW_MS: u64 = 10_000;
const LIMITED_WAIT_MS: u64 = 5_000;

/// How long after losing the name it is probed for again: a minute. A host
/// that holds the name answers one probe a minute for each host that wants
/// it, twelve times fewer than §8.1's five seconds would ask of it, and a
/// name whose holder has left is back within the minute and the second a
/// probing takes. §8.1's five seconds after a failed attempt are inside it.
const RETRY_MS: u64 = 60_000;

/// §6: a record is multicast on an interface at most once a second.
const GROUP_EVERY_MS: u64 = 1_000;

/// §6: "when responding via multicast to a probe, a Multicast DNS responder
/// is only required to delay its transmission as necessary to ensure an
/// interval of at least 250 ms since the last time the record was multicast
/// on that interface."
const PROBE_ANSWER_EVERY_MS: u64 = 250;

/// §8.3: "The Multicast DNS responder MUST send at least two unsolicited
/// responses, one second apart."
const ANNOUNCE_AGAIN_MS: u64 = 1_000;

/// Whose the name is, read only while a link is held.
#[derive(Clone, Copy, Debug)]
enum Claim {
    /// Probing is owed from `from_ms` on: the next [`Responder::owed`] draws
    /// its delay.
    Owed { from_ms: u64 },
    /// `sent` probes have left; at `at_ms` the next does, or after the third
    /// the name is held. A message conflicts only once `sent` is not zero.
    Probing { sent: u8, at_ms: u64 },
    Held,
}

/// This host's one record on its link, on the caller's monotonic clock in
/// milliseconds: probed for on every link after none and [`RETRY_MS`] after
/// each loss, then announced twice a second apart (§8, §8.3), answered to
/// whoever asks, and multicast at most once a second (§6), so no host can
/// turn the queries it sends into a multicast to every host on the link at
/// its own rate. A query §6 holds back is answered when the second ends, not
/// dropped: the caller wakes at [`Responder::owed_at`], for that and for
/// every probe, a retry's included.
///
/// A message counts as sent once [`Responder::owed`] has handed it over.
#[derive(Debug)]
pub struct Responder<'a> {
    host: Host<'a>,
    /// The link the address is held on.
    link: Option<Link>,
    claim: Claim,
    /// [`Event::Lost`] was said, and [`Event::Claimed`] not since.
    lost: bool,
    /// When the name last met a conflict.
    conflict_ms: Option<u64>,
    last_group_ms: Option<u64>,
    /// When a held record is next owed to the group: an announcement, or an
    /// answer §6 delayed. One multicast of the record answers every query it
    /// held.
    owed_ms: Option<u64>,
    /// The multicast owed at `owed_ms` is the first of §8.3's two: the second
    /// is owed a second after it.
    again: bool,
}

impl<'a> Responder<'a> {
    pub const fn new(host: Host<'a>) -> Self {
        Self { host, link: None, claim: Claim::Owed { from_ms: 0 }, lost: false, conflict_ms: None, last_group_ms: None, owed_ms: None, again: false }
    }

    /// This host is on `link` at `now_ms`, or on none while it holds no
    /// address or its link is down. A link after none owes a probing (§8,
    /// §10.2), whoever held the name; a new address under a held name owes
    /// its announcement (§8.4).
    pub fn on(&mut self, link: Option<Link>, now_ms: u64) {
        let Some(link) = link else {
            self.link = None;
            self.owed_ms = None;
            return;
        };
        match self.link.replace(link) {
            None => self.release(Claim::Owed { from_ms: now_ms.saturating_add(self.limit_ms(now_ms)) }),
            Some(was) if was.addr != link.addr && matches!(self.claim, Claim::Held) => {
                self.owed_ms = Some(now_ms);
                self.again = true;
            }
            Some(_) => {}
        }
    }

    /// The multicast owed at `now_ms`, if any — a probe, an announcement of a
    /// name just claimed or of a new address under a held one, its second, or
    /// an answer §6 delayed — and whether this call claimed the name. Asked
    /// after every message that has arrived was [`Self::heard`]. `delay` is
    /// drawn once when this call starts a probing, and not otherwise.
    pub fn owed(&mut self, now_ms: u64, delay: impl FnOnce() -> u32) -> (Option<Vec<u8>>, Option<Event>) {
        let Some(link) = self.link else { return (None, None) };
        if let Claim::Owed { from_ms } = self.claim {
            let wait = u64::from(delay() % (PROBE_DELAY_MS + 1));
            self.claim = Claim::Probing { sent: 0, at_ms: from_ms.max(now_ms).saturating_add(wait) };
        }
        let mut event = None;
        if let Claim::Probing { sent, at_ms } = self.claim {
            if now_ms < at_ms {
                return (None, None);
            }
            if sent < PROBES {
                self.claim = Claim::Probing { sent: sent + 1, at_ms: now_ms.saturating_add(PROBE_EVERY_MS) };
                return (Some(probe(self.host, link.addr)), None);
            }
            self.claim = Claim::Held;
            self.lost = false;
            event = Some(Event::Claimed);
            self.owed_ms = Some(self.group_free_ms(now_ms, GROUP_EVERY_MS));
            self.again = true;
        }
        if !matches!(self.claim, Claim::Held) || !self.owed_ms.is_some_and(|at| now_ms >= at) {
            return (None, event);
        }
        self.owed_ms = core::mem::take(&mut self.again).then(|| now_ms.saturating_add(ANNOUNCE_AGAIN_MS));
        self.last_group_ms = Some(now_ms);
        (Some(announcement(self.host, link.addr)), event)
    }

    /// The name is no longer held: nothing of the record is owed.
    fn release(&mut self, claim: Claim) {
        self.claim = claim;
        self.owed_ms = None;
        self.again = false;
    }

    /// What §8.1's limit puts before a probe attempt that would begin at
    /// `now_ms`: five seconds within ten of a conflict.
    fn limit_ms(&self, now_ms: u64) -> u64 {
        let limited = self.conflict_ms.is_some_and(|last| now_ms.saturating_sub(last) <= CONFLICT_WINDOW_MS);
        if limited {
            LIMITED_WAIT_MS
        } else {
            0
        }
    }

    /// A conflict at `now_ms`: what §8.1's limit puts before the probe
    /// attempt that follows it.
    fn conflict(&mut self, now_ms: u64) -> u64 {
        let wait = self.limit_ms(now_ms);
        self.conflict_ms = Some(now_ms);
        wait
    }

    /// When the record was last multicast plus `every_ms`, or `now_ms` if
    /// that is later: when §6 next lets it be multicast.
    fn group_free_ms(&self, now_ms: u64, every_ms: u64) -> u64 {
        self.last_group_ms.map_or(now_ms, |last| last.saturating_add(every_ms)).max(now_ms)
    }

    /// When [`Self::owed`] is next owed a call, on the caller's clock.
    pub fn owed_at(&self) -> Option<u64> {
        self.link?;
        match self.claim {
            Claim::Owed { from_ms } => Some(from_ms),
            Claim::Probing { at_ms, .. } => Some(at_ms),
            Claim::Held => self.owed_ms,
        }
    }

    /// `message` arrived from `from` at `now_ms`: the answer it is owed now,
    /// if it is a query for a held name, and whether it took the name; a
    /// response or another host's probe is read for what it says of the name
    /// and answered nothing. No answer too where it asks nothing this host
    /// answers — no link held, a source off the link, an opcode or a response
    /// code other than zero, a question for another name or type, or bytes
    /// that are not a message at all — or where §6 delays the answer to
    /// [`Responder::owed_at`].
    pub fn heard(&mut self, message: &[u8], from: Source, now_ms: u64) -> (Option<Answer>, Option<Event>) {
        let Some(link) = self.link.filter(|link| link.holds(from.addr)) else { return (None, None) };
        let (Ok(id), Ok(flags)) = (u16_at(message, 0), u16_at(message, 2)) else { return (None, None) };
        if flags & (OPCODE_MASK | RCODE_MASK) != 0 {
            return (None, None);
        }
        if flags & QR != 0 {
            return (None, self.response(message, from, link, now_ms));
        }
        match self.claim {
            Claim::Held => (self.answer(message, id, from, link, now_ms), None),
            Claim::Probing { sent, .. } if sent > 0 => {
                self.rival(message, link, now_ms);
                (None, None)
            }
            Claim::Owed { .. } | Claim::Probing { .. } => (None, None),
        }
    }

    /// A response: §6 has one from a port other than 5353 silently ignored,
    /// and one that conflicts takes a name under probe (§8.1), which is
    /// probed for again [`RETRY_MS`] later, and sends a held one back to
    /// probing (§9).
    fn response(&mut self, message: &[u8], from: Source, link: Link, now_ms: u64) -> Option<Event> {
        let probing = match self.claim {
            Claim::Probing { sent, .. } if sent > 0 => true,
            Claim::Held => false,
            Claim::Owed { .. } | Claim::Probing { .. } => return None,
        };
        if from.port != PORT || !conflicts(message, self.host, link.addr).unwrap_or(false) {
            return None;
        }
        let wait = self.conflict(now_ms);
        if probing {
            self.release(Claim::Owed { from_ms: now_ms.saturating_add(RETRY_MS) });
            return (!core::mem::replace(&mut self.lost, true)).then_some(Event::Lost);
        }
        self.release(Claim::Owed { from_ms: now_ms.saturating_add(wait) });
        None
    }

    /// A query heard under this host's probe: §8.2's comparison if it is
    /// another host's probe for the name. A lost name defers for
    /// [`RETRY_MS`], as it does to an answer.
    fn rival(&mut self, query: &[u8], link: Link, now_ms: u64) {
        let Some((kind, _)) = asked(query, self.host) else { return };
        let Some((theirs, more)) = proposed(query, self.host, kind) else { return };
        // §8.2.1: equal as far as this host's one record goes, "the list with
        // records remaining is deemed to have won"; with none remaining "there
        // is, in fact, no conflict."
        let later = match theirs.cmp(&(CLASS_IN, TYPE_A, &link.addr[..])) {
            Ordering::Greater => true,
            Ordering::Equal => more,
            Ordering::Less => false,
        };
        if later {
            let wait = self.conflict(now_ms).max(if self.lost { RETRY_MS } else { DEFER_MS });
            self.release(Claim::Probing { sent: 0, at_ms: now_ms.saturating_add(wait) });
        }
    }

    /// A query for a held name.
    fn answer(&mut self, query: &[u8], id: u16, from: Source, link: Link, now_ms: u64) -> Option<Answer> {
        let host = self.host;
        let (kind, unicast) = asked(query, host)?;
        if from.port != PORT {
            // §6.7: the asker's ID and question, and a record no cache keeps long.
            let mut out = header(id, RESPONSE_FLAGS, [1, 1, 0]);
            host.put_name(&mut out);
            out.extend_from_slice(&TYPE_A.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
            answer_record(&mut out, host, link.addr, CLASS_IN, LEGACY_TTL);
            return Some(Answer { to: To::Asker, bytes: out });
        }
        if !unicast {
            // §6: "A probe query can be distinguished from a normal query by
            // the fact that a probe query contains a proposed record in the
            // Authority Section that answers the question".
            let every_ms = if proposed(query, host, kind).is_some() { PROBE_ANSWER_EVERY_MS } else { GROUP_EVERY_MS };
            let free_ms = self.group_free_ms(now_ms, every_ms);
            if now_ms < free_ms {
                self.owed_ms = Some(self.owed_ms.map_or(free_ms, |owed| owed.min(free_ms)));
                return None;
            }
            self.last_group_ms = Some(now_ms);
        }
        Some(Answer { to: if unicast { To::Asker } else { To::Group }, bytes: announcement(host, link.addr) })
    }
}

/// The first question of `query` this host's record answers: its type, and
/// whether it asks for a unicast response (§5.4).
fn asked(query: &[u8], host: Host) -> Option<(u16, bool)> {
    let mut at = HEADER;
    for _ in 0..u16_at(query, 4).ok()? {
        let (name, after) = name_at(query, at).ok()?;
        let kind = u16_at(query, after).ok()?;
        let class = u16_at(query, after + 2).ok()?;
        at = after + 4;
        if host.is(&name) && matches!(kind, TYPE_A | TYPE_ANY) && class & !UNICAST_RESPONSE == CLASS_IN {
            return Some((kind, class & UNICAST_RESPONSE != 0));
        }
    }
    None
}

/// One resource record (RFC 1035 §4.1.3) and where the bytes after it begin.
struct Record<'m> {
    name: Name,
    kind: u16,
    class: u16,
    data: &'m [u8],
    end: usize,
}

fn record_at(message: &[u8], at: usize) -> Option<Record<'_>> {
    let (name, after) = name_at(message, at).ok()?;
    let kind = u16_at(message, after).ok()?;
    let class = u16_at(message, after + 2).ok()?;
    let end = after + 10 + usize::from(u16_at(message, after + 8).ok()?);
    Some(Record { name, kind, class, data: message.get(after + 10..end)?, end })
}

/// Where the records of `message` begin: behind its questions.
fn records_at(message: &[u8]) -> Option<usize> {
    let mut at = HEADER;
    for _ in 0..u16_at(message, 4).ok()? {
        at = name_at(message, at).ok()?.1 + 4;
    }
    Some(at)
}

/// Whether `response` carries, in any of its three record sections (§9), a
/// record of `host`'s name other than its `A` record for `addr`. `None` where
/// it stops being a message before one is found.
fn conflicts(response: &[u8], host: Host, addr: [u8; 4]) -> Option<bool> {
    let mut at = records_at(response)?;
    let records = [6, 8, 10].into_iter().map(|count| u16_at(response, count).map(u32::from));
    for _ in 0..records.sum::<Result<u32, _>>().ok()? {
        let record = record_at(response, at)?;
        at = record.end;
        let ours = record.kind == TYPE_A && record.class & !CACHE_FLUSH == CLASS_IN && record.data == addr;
        if host.is(&record.name) && !ours {
            return Some(true);
        }
    }
    Some(false)
}

/// A record as §8.2 orders it: "first comparing the record class (excluding
/// the cache-flush bit described in Section 10.2), then the record type, then
/// raw comparison of the binary content of the rdata", its bytes "as
/// eight-bit UNSIGNED values", the shorter of two that agree the earlier.
///
/// The data is compared as the message carries it, and §8.2 has a name in it
/// uncompressed first: none is, because data is reached only between two
/// records of one class and type, this host's is its `A` record, and an
/// address holds no name.
type Proposed<'m> = (u16, u16, &'m [u8]);

/// The records `query` proposes in its Authority Section for `host`'s name
/// that answer a question of type `kind`: the first of them as §8.2.1 sorts
/// them, and whether there is another. `None` where it proposes none, which
/// is what makes a query no probe (§6), or is no message that far.
fn proposed<'m>(query: &'m [u8], host: Host, kind: u16) -> Option<(Proposed<'m>, bool)> {
    let mut at = records_at(query)?;
    for _ in 0..u16_at(query, 6).ok()? {
        at = record_at(query, at)?.end;
    }
    let mut first: Option<Proposed> = None;
    let mut more = false;
    for _ in 0..u16_at(query, 8).ok()? {
        let record = record_at(query, at)?;
        at = record.end;
        if host.is(&record.name) && (kind == TYPE_ANY || kind == record.kind) {
            let theirs = (record.class & !CACHE_FLUSH, record.kind, record.data);
            more = first.is_some();
            if first.is_none_or(|first| theirs < first) {
                first = Some(theirs);
            }
        }
    }
    first.map(|first| (first, more))
}

/// A header (RFC 1035 §4.1.1) with `counts` questions, answers and authority
/// records, and no additional one.
fn header(id: u16, flags: u16, counts: [u16; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    for word in [id, flags, counts[0], counts[1], counts[2], 0] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out
}

fn answer_record(out: &mut Vec<u8>, host: Host, addr: [u8; 4], class: u16, ttl: u32) {
    host.put_name(out);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&addr);
}
