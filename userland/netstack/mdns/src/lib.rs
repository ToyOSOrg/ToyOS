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
//! Where the answer goes follows the question (§5.4, §6.7):
//!
//! - a query from port 5353 is a full responder's: the answer is multicast to
//!   the group, unless it set the unicast-response bit, which asks for it back
//!   at its own address;
//! - a query from any other port is a legacy resolver's (§6.7): the answer goes
//!   back to its address and port, carrying its ID and its question, with no
//!   cache-flush bit and a TTL of at most ten seconds.
//!
//! A query whose source is not on this link is ignored (§11), and the record
//! is multicast at most once a second (§6) — announcements included — with a
//! query inside that second answered when it ends ([`Responder`]).
//!
//! **Not implemented, and so not claimed:** probing for the name before using
//! it (§8.1) and defending it against another host's (§9). Two machines named
//! alike both answer, and a resolver may see either.
//!
//! A question is read off the wire by `toyos-dns`, the reader a resolver's
//! reply is read by.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

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
/// responder is the only owner of the name it answers for.
const CACHE_FLUSH: u16 = 0x8000;
/// `QR` and `AA`: a response, authoritative (§18.2, §18.4).
const RESPONSE_FLAGS: u16 = 0x8400;
/// §18.3: the opcode of any query this answers is zero.
const OPCODE_MASK: u16 = 0x7800;
const QR: u16 = 0x8000;

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

    /// Whether `name`, as a question spelled it, is this host's, whatever the
    /// case of its letters.
    fn is(&self, name: &Name) -> bool {
        let mut ours = Vec::with_capacity(self.0.len() + LOCAL.len() + 3);
        self.put_name(&mut ours);
        name.wire().eq_ignore_ascii_case(&ours)
    }
}

/// The unsolicited answer a responder sends when its address is new (§8.3).
pub fn announcement(host: Host, addr: [u8; 4]) -> Vec<u8> {
    let mut out = header(0, RESPONSE_FLAGS, 0, 1);
    answer_record(&mut out, host, addr, CLASS_IN | CACHE_FLUSH, TTL);
    out
}

/// Where a query came from: the source address and port of its packet.
#[derive(Clone, Copy, Debug)]
pub struct Asker {
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
    /// Whether a query from `addr` is not from off this link, which is what
    /// §11 refuses: in this subnet, or link-local (RFC 3927). A loopback
    /// source is off every link (RFC 1122 §3.2.1.3: a host silently discards
    /// a datagram carrying one), so it is refused wherever it arrived.
    fn holds(&self, addr: [u8; 4]) -> bool {
        let mask = u32::MAX.checked_shl(32 - u32::from(self.prefix.min(32))).unwrap_or(0);
        let (ours, theirs) = (u32::from_be_bytes(self.addr), u32::from_be_bytes(addr));
        ours & mask == theirs & mask || addr[..2] == [169, 254]
    }
}

/// §6: a record is multicast on an interface at most once a second.
const GROUP_EVERY_MS: u64 = 1_000;

/// §8.3: "The Multicast DNS responder MUST send at least two unsolicited
/// responses, one second apart."
const ANNOUNCE_AGAIN_MS: u64 = 1_000;

/// This host's one record on its link, on the caller's monotonic clock in
/// milliseconds: announced on every new address and a second later (§8.3),
/// answered to whoever asks, and multicast at most once a second (§6), so no
/// host can turn the queries it sends into a multicast to every host on the
/// link at its own rate. A query §6 holds back is answered when the second
/// ends, not dropped: the caller wakes at [`Responder::owed_at`].
#[derive(Debug)]
pub struct Responder<'a> {
    host: Host<'a>,
    /// The link the record was last announced on.
    link: Option<Link>,
    last_group_ms: Option<u64>,
    /// When the record is next owed to the group: the second announcement,
    /// or an answer §6 delayed. One multicast of the record answers every
    /// query it held.
    owed_ms: Option<u64>,
}

impl<'a> Responder<'a> {
    pub const fn new(host: Host<'a>) -> Self {
        Self { host, link: None, last_group_ms: None, owed_ms: None }
    }

    /// This host is on `link` at `now_ms`, or on none while it holds no
    /// address: the multicast the record is owed now, if any — a new
    /// address's announcement, its second, or an answer §6 delayed.
    pub fn on(&mut self, link: Option<Link>, now_ms: u64) -> Option<Vec<u8>> {
        let Some(link) = link else {
            self.link = None;
            self.owed_ms = None;
            return None;
        };
        let new = self.link.is_none_or(|was| was.addr != link.addr);
        self.link = Some(link);
        if new {
            self.owed_ms = Some(now_ms.saturating_add(ANNOUNCE_AGAIN_MS));
        } else if self.owed_ms.is_some_and(|at| now_ms >= at) {
            self.owed_ms = None;
        } else {
            return None;
        }
        self.last_group_ms = Some(now_ms);
        Some(announcement(self.host, link.addr))
    }

    /// When the record is next owed to the group, on the caller's clock.
    pub fn owed_at(&self) -> Option<u64> {
        self.owed_ms
    }

    /// The answer `query`, from `asker`, is owed now, or `None` where it asks
    /// nothing this host answers — no address held, a source off the link, a
    /// response, a query with a nonzero opcode, a question for another name or
    /// type, or bytes that are not a message at all — or where §6 delays it
    /// to [`Responder::owed_at`].
    pub fn answer(&mut self, query: &[u8], asker: Asker, now_ms: u64) -> Option<Answer> {
        let link = self.link?;
        if !link.holds(asker.addr) {
            return None;
        }
        let host = self.host;
        let id = u16_at(query, 0).ok()?;
        let flags = u16_at(query, 2).ok()?;
        if flags & (QR | OPCODE_MASK) != 0 {
            return None;
        }
        let questions = u16_at(query, 4).ok()?;
        let mut at = 12;
        let mut asked = None;
        for _ in 0..questions {
            let (name, after) = name_at(query, at).ok()?;
            let kind = u16_at(query, after).ok()?;
            let class = u16_at(query, after + 2).ok()?;
            at = after + 4;
            if host.is(&name)
                && matches!(kind, TYPE_A | TYPE_ANY)
                && class & !UNICAST_RESPONSE == CLASS_IN
            {
                asked = Some(class & UNICAST_RESPONSE != 0);
                break;
            }
        }
        let unicast = asked?;
        if asker.port != PORT {
            // §6.7: the asker's ID and question, and a record no cache keeps long.
            let mut out = header(id, RESPONSE_FLAGS, 1, 1);
            host.put_name(&mut out);
            out.extend_from_slice(&TYPE_A.to_be_bytes());
            out.extend_from_slice(&CLASS_IN.to_be_bytes());
            answer_record(&mut out, host, link.addr, CLASS_IN, LEGACY_TTL);
            return Some(Answer { to: To::Asker, bytes: out });
        }
        if !unicast {
            let free_ms = self.last_group_ms.map_or(now_ms, |last| last.saturating_add(GROUP_EVERY_MS));
            if now_ms < free_ms {
                self.owed_ms = Some(free_ms);
                return None;
            }
            self.last_group_ms = Some(now_ms);
        }
        let mut out = header(0, RESPONSE_FLAGS, 0, 1);
        answer_record(&mut out, host, link.addr, CLASS_IN | CACHE_FLUSH, TTL);
        Some(Answer { to: if unicast { To::Asker } else { To::Group }, bytes: out })
    }
}

fn header(id: u16, flags: u16, questions: u16, answers: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    for word in [id, flags, questions, answers, 0, 0] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    const ADDR: [u8; 4] = [192, 168, 1, 49];

    fn host() -> Host<'static> {
        Host::new("toyos-t14").expect("a label")
    }

    const LINK: Link = Link { addr: ADDR, prefix: 24 };
    const NEIGHBOUR: [u8; 4] = [192, 168, 1, 7];

    /// A responder that announced [`LINK`]'s address at 0.
    fn announced() -> Responder<'static> {
        let mut r = Responder::new(host());
        assert!(r.on(Some(LINK), 0).is_some(), "a new address is announced");
        r
    }

    /// One query from a neighbour on the link, long after the announcement.
    fn ask(query: &[u8], port: u16) -> Option<Answer> {
        announced().answer(query, Asker { addr: NEIGHBOUR, port }, 10_000)
    }

    /// A query as RFC 1035 §4.1 lays one out, spelled byte by byte here rather
    /// than by this crate's own writer.
    fn query(id: u16, name: &[&str], kind: u16, class: u16) -> Vec<u8> {
        let mut q = vec![(id >> 8) as u8, id as u8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&kind.to_be_bytes());
        q.extend_from_slice(&class.to_be_bytes());
        q
    }

    /// The answer a multicast query is owed, byte for byte: RFC 6762's header
    /// (§18: ID zero, `QR|AA`, no question), one `A` record with the
    /// cache-flush bit and a 120 s TTL.
    #[test]
    fn a_query_for_this_name_is_answered_to_the_group() {
        let got = ask(&query(0, &["toyos-t14", "local"], 1, 1), PORT)
            .expect("an answer");
        assert_eq!(got.to, To::Group);
        let mut want = vec![0, 0, 0x84, 0x00, 0, 0, 0, 1, 0, 0, 0, 0];
        want.extend_from_slice(b"\x09toyos-t14\x05local\x00");
        want.extend_from_slice(&[0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4, 192, 168, 1, 49]);
        assert_eq!(got.bytes, want);
        assert_eq!(announcement(host(), ADDR), want, "the announcement is the same answer, unasked");
    }

    #[test]
    fn a_name_is_matched_whatever_its_case_and_any_asks_for_it_too() {
        let q = query(0, &["ToyOS-T14", "LOCAL"], 255, 1);
        assert!(ask(&q, PORT).is_some());
    }

    #[test]
    fn the_unicast_bit_sends_the_answer_back_to_the_asker() {
        let got = ask(&query(0, &["toyos-t14", "local"], 1, 0x8001), PORT)
            .expect("an answer");
        assert_eq!(got.to, To::Asker);
    }

    /// §6.7: a resolver that is not a responder gets its ID, its question and
    /// a short TTL, and no cache-flush bit.
    #[test]
    fn a_legacy_resolver_gets_its_id_its_question_and_a_short_ttl() {
        let got = ask(&query(0xBEEF, &["toyos-t14", "local"], 1, 1), 53_000)
            .expect("an answer");
        assert_eq!(got.to, To::Asker);
        let mut want = vec![0xBE, 0xEF, 0x84, 0x00, 0, 1, 0, 1, 0, 0, 0, 0];
        want.extend_from_slice(b"\x09toyos-t14\x05local\x00\x00\x01\x00\x01");
        want.extend_from_slice(b"\x09toyos-t14\x05local\x00");
        want.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 10, 0, 4, 192, 168, 1, 49]);
        assert_eq!(got.bytes, want);
    }

    #[test]
    fn a_question_that_is_not_this_hosts_is_not_answered() {
        for (name, kind, class) in [
            (&["other", "local"][..], 1, 1),
            (&["toyos-t14", "lan"][..], 1, 1),
            (&["toyos-t14"][..], 1, 1),
            (&["toyos-t14", "local", "x"][..], 1, 1),
            (&["toyos-t14", "local"][..], 28, 1),
            (&["toyos-t14", "local"][..], 1, 3),
        ] {
            assert_eq!(ask(&query(0, name, kind, class), PORT), None, "{name:?}");
        }
        let mut response = query(0, &["toyos-t14", "local"], 1, 1);
        response[2] = 0x84;
        assert_eq!(ask(&response, PORT), None, "a response is not a question");
        let mut opcode = query(0, &["toyos-t14", "local"], 1, 1);
        opcode[2] = 0x08;
        assert_eq!(ask(&opcode, PORT), None, "a nonzero opcode is not a query");
    }

    /// A second question may name the first's labels by pointer (RFC 1035
    /// §4.1.4), as a responder asking several things at once does.
    #[test]
    fn a_question_spelled_by_a_pointer_is_read() {
        let mut q = query(0, &["other", "local"], 1, 1);
        q[5] = 2;
        // `toyos-t14`, then a pointer to `local` at offset 12 + 6.
        q.extend_from_slice(b"\x09toyos-t14\xC0\x12\x00\x01\x00\x01");
        assert_eq!(ask(&q, PORT).map(|a| a.to), Some(To::Group));
    }

    /// Nothing a peer sends can hold the parser: a short message, a label past
    /// the end, a pointer forwards or to itself are each no question.
    #[test]
    fn a_malformed_message_is_no_question() {
        let whole = query(0, &["toyos-t14", "local"], 1, 1);
        for cut in 0..whole.len() {
            assert_eq!(ask(&whole[..cut], PORT), None, "cut at {cut}");
        }
        let mut forward = vec![0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        forward.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1]);
        assert_eq!(ask(&forward, PORT), None, "a pointer to itself");
        let mut ahead = vec![0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        ahead.extend_from_slice(&[0xC0, 20, 0, 1, 0, 1, 0, 0]);
        assert_eq!(ask(&ahead, PORT), None, "a pointer forwards");
        let mut reserved = vec![0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        reserved.extend_from_slice(&[0x40, 0, 0, 1, 0, 1]);
        assert_eq!(ask(&reserved, PORT), None, "a reserved label type");
    }

    /// RFC 6762 §6: "a Multicast DNS responder MUST NOT (except in the one
    /// special case of answering probe queries) multicast a record on a given
    /// interface until at least one second has elapsed since the last time that
    /// record was multicast on that particular interface." An announcement is a
    /// multicast of the record too. A query inside the second is answered when
    /// it ends, by one multicast of the record, and not before or never.
    #[test]
    fn the_record_is_multicast_at_most_once_a_second_and_a_query_inside_it_waits() {
        let q = query(0, &["toyos-t14", "local"], 1, 1);
        let from = Asker { addr: NEIGHBOUR, port: PORT };
        let mut r = Responder::new(host());
        let record = r.on(Some(LINK), 20_000).expect("a new address is announced");
        assert_eq!(r.owed_at(), Some(21_000), "§8.3: the second announcement");
        assert_eq!(r.answer(&q, from, 20_500), None, "the record was announced 500 ms ago");
        let legacy = Asker { addr: NEIGHBOUR, port: 53_000 };
        assert_eq!(
            r.answer(&q, legacy, 20_500).map(|a| a.to),
            Some(To::Asker),
            "a unicast answer is no multicast of the record"
        );
        assert_eq!(r.on(Some(LINK), 20_999), None);
        assert_eq!(r.on(Some(LINK), 21_000).as_ref(), Some(&record), "announced again, which answers the query");
        assert_eq!(r.owed_at(), None);

        assert_eq!(r.answer(&q, from, 21_400), None, "the record was multicast 400 ms ago");
        assert_eq!(r.answer(&q, from, 21_600), None);
        assert_eq!(r.owed_at(), Some(22_000), "both queries are owed one multicast when the second ends");
        assert_eq!(r.on(Some(LINK), 21_999), None);
        assert_eq!(r.on(Some(LINK), 22_000).as_ref(), Some(&record));
        assert_eq!(r.on(Some(LINK), 22_001), None, "owed once");

        let later = r.answer(&q, from, 23_000).map(|a| a.to);
        assert_eq!(later, Some(To::Group), "a second has passed");
        assert_eq!(r.answer(&q, from, 23_999), None);
        assert_eq!(r.owed_at(), Some(24_000));
    }

    /// Nothing is answered without an address; an address after none, or a
    /// different one, is new and announced at once with what it is.
    #[test]
    fn an_address_is_announced_when_new_and_none_is_answered_for() {
        let q = query(0, &["toyos-t14", "local"], 1, 1);
        let from = Asker { addr: NEIGHBOUR, port: 53_000 };
        let mut r = Responder::new(host());
        assert_eq!(r.answer(&q, from, 0), None, "no address yet");
        assert!(r.on(Some(LINK), 0).is_some());
        assert_eq!(r.on(Some(LINK), 500), None);
        assert_eq!(r.on(None, 600), None);
        assert_eq!(r.owed_at(), None, "nothing is owed for an address no longer held");
        assert_eq!(r.answer(&q, from, 600), None, "no address any more");
        assert!(r.on(Some(LINK), 700).is_some(), "an address after none is new");
        let moved = Link { addr: [192, 168, 1, 50], prefix: 24 };
        let announced = r.on(Some(moved), 800).expect("a different address is new");
        assert_eq!(announced[announced.len() - 4..], [192, 168, 1, 50]);
    }

    /// RFC 6762 §11: a query whose source is not on this link is ignored; a
    /// link-local source (RFC 3927) is on every link. RFC 1122 §3.2.1.3: a
    /// loopback source is never on a wire, and a datagram carrying one is
    /// silently discarded.
    #[test]
    fn a_query_from_off_the_link_is_not_answered() {
        let q = query(0, &["toyos-t14", "local"], 1, 1);
        for addr in [[10, 0, 0, 7], [192, 168, 2, 7], [8, 8, 8, 8], [127, 0, 0, 1], [127, 1, 2, 3]] {
            for port in [PORT, 53_000] {
                assert_eq!(announced().answer(&q, Asker { addr, port }, 10_000), None, "{addr:?}:{port}");
            }
        }
        for addr in [[192, 168, 1, 254], [169, 254, 3, 4]] {
            let asked = announced().answer(&q, Asker { addr, port: PORT }, 10_000);
            assert!(asked.is_some(), "{addr:?}");
        }
    }

    #[test]
    fn a_host_name_is_one_label() {
        assert!(Host::new("toyos-t14").is_ok());
        for bad in ["", "a.b", "-a", "a-", "a b", "é", &"a".repeat(64)] {
            assert!(Host::new(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_group_address_maps_to_its_ethernet_address() {
        assert_eq!(GROUP_MAC, [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb]);
    }
}
