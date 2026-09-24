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
//! **Not implemented, and so not claimed:** probing for the name before using
//! it (§8.1) and defending it against another host's (§9). Two machines named
//! alike both answer, and a resolver may see either.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

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

/// The longest name the wire form carries (RFC 1035 §2.3.4), and a label's.
const MAX_NAME: usize = 255;
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

    /// Whether `name`, as a question spelled it, is this host's.
    fn is(&self, name: &[&[u8]]) -> bool {
        matches!(name, [host, local] if host.eq_ignore_ascii_case(self.0.as_bytes()) && local.eq_ignore_ascii_case(LOCAL))
    }
}

/// The unsolicited answer a responder sends when its address is new (§8.3).
pub fn announcement(host: Host, addr: [u8; 4]) -> Vec<u8> {
    let mut out = header(0, RESPONSE_FLAGS, 0, 1);
    answer_record(&mut out, host, addr, CLASS_IN | CACHE_FLUSH, TTL);
    out
}

/// The answer `query`, from port `from_port`, is owed for `host` at `addr`, or
/// `None` where it asks nothing this host answers — a response, a query with a
/// nonzero opcode, a question for another name or type, or bytes that are not
/// a message at all.
pub fn answer(query: &[u8], from_port: u16, host: Host, addr: [u8; 4]) -> Option<Answer> {
    let id = u16_at(query, 0)?;
    let flags = u16_at(query, 2)?;
    if flags & (QR | OPCODE_MASK) != 0 {
        return None;
    }
    let questions = u16_at(query, 4)?;
    let mut at = 12;
    let mut asked = None;
    for _ in 0..questions {
        let mut labels: [&[u8]; 8] = [&[]; 8];
        let (count, after) = name_at(query, at, &mut labels)?;
        let kind = u16_at(query, after)?;
        let class = u16_at(query, after + 2)?;
        at = after + 4;
        if host.is(&labels[..count])
            && matches!(kind, TYPE_A | TYPE_ANY)
            && class & !UNICAST_RESPONSE == CLASS_IN
        {
            asked = Some(class & UNICAST_RESPONSE != 0);
            break;
        }
    }
    let unicast = asked?;
    if from_port != PORT {
        // §6.7: the asker's ID and question, and a record no cache keeps long.
        let mut out = header(id, RESPONSE_FLAGS, 1, 1);
        host.put_name(&mut out);
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        answer_record(&mut out, host, addr, CLASS_IN, LEGACY_TTL);
        return Some(Answer { to: To::Asker, bytes: out });
    }
    let mut out = header(0, RESPONSE_FLAGS, 0, 1);
    answer_record(&mut out, host, addr, CLASS_IN | CACHE_FLUSH, TTL);
    Some(Answer { to: if unicast { To::Asker } else { To::Group }, bytes: out })
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

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at.checked_add(1)?)?]))
}

/// The name at `at`, its labels into `labels`: how many there are, and where
/// the bytes after the name begin. Follows compression pointers (RFC 1035
/// §4.1.4) backwards only, so a loop of pointers cannot hold it; a name with
/// more labels than `labels` holds is no name this host answers to.
fn name_at<'a>(bytes: &'a [u8], at: usize, labels: &mut [&'a [u8]]) -> Option<(usize, usize)> {
    let mut count = 0;
    let mut here = at;
    let mut after = None;
    let mut spelled = 0usize;
    loop {
        let len = *bytes.get(here)? as usize;
        match len {
            0 => return Some((count, after.unwrap_or(here + 1))),
            1..=MAX_LABEL => {
                let label = bytes.get(here + 1..here + 1 + len)?;
                spelled += len + 1;
                if spelled > MAX_NAME {
                    return None;
                }
                *labels.get_mut(count)? = label;
                count += 1;
                here += 1 + len;
            }
            0xC0..=0xFF => {
                let target = u16_at(bytes, here)? as usize & 0x3FFF;
                if target >= here {
                    return None;
                }
                after.get_or_insert(here + 2);
                here = target;
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    const ADDR: [u8; 4] = [192, 168, 1, 49];

    fn host() -> Host<'static> {
        Host::new("toyos-t14").expect("a label")
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
        let got = answer(&query(0, &["toyos-t14", "local"], 1, 1), PORT, host(), ADDR)
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
        assert!(answer(&q, PORT, host(), ADDR).is_some());
    }

    #[test]
    fn the_unicast_bit_sends_the_answer_back_to_the_asker() {
        let got = answer(&query(0, &["toyos-t14", "local"], 1, 0x8001), PORT, host(), ADDR)
            .expect("an answer");
        assert_eq!(got.to, To::Asker);
    }

    /// §6.7: a resolver that is not a responder gets its ID, its question and
    /// a short TTL, and no cache-flush bit.
    #[test]
    fn a_legacy_resolver_gets_its_id_its_question_and_a_short_ttl() {
        let got = answer(&query(0xBEEF, &["toyos-t14", "local"], 1, 1), 53_000, host(), ADDR)
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
            assert_eq!(answer(&query(0, name, kind, class), PORT, host(), ADDR), None, "{name:?}");
        }
        let mut response = query(0, &["toyos-t14", "local"], 1, 1);
        response[2] = 0x84;
        assert_eq!(answer(&response, PORT, host(), ADDR), None, "a response is not a question");
        let mut opcode = query(0, &["toyos-t14", "local"], 1, 1);
        opcode[2] = 0x08;
        assert_eq!(answer(&opcode, PORT, host(), ADDR), None, "a nonzero opcode is not a query");
    }

    /// A second question may name the first's labels by pointer (RFC 1035
    /// §4.1.4), as a responder asking several things at once does.
    #[test]
    fn a_question_spelled_by_a_pointer_is_read() {
        let mut q = query(0, &["other", "local"], 1, 1);
        q[5] = 2;
        // `toyos-t14`, then a pointer to `local` at offset 12 + 6.
        q.extend_from_slice(b"\x09toyos-t14\xC0\x12\x00\x01\x00\x01");
        assert_eq!(answer(&q, PORT, host(), ADDR).map(|a| a.to), Some(To::Group));
    }

    /// Nothing a peer sends can hold the parser: a short message, a label past
    /// the end, a pointer forwards or to itself are each no question.
    #[test]
    fn a_malformed_message_is_no_question() {
        let whole = query(0, &["toyos-t14", "local"], 1, 1);
        for cut in 0..whole.len() {
            assert_eq!(answer(&whole[..cut], PORT, host(), ADDR), None, "cut at {cut}");
        }
        let mut forward = vec![0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        forward.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1]);
        assert_eq!(answer(&forward, PORT, host(), ADDR), None, "a pointer to itself");
        let mut ahead = vec![0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        ahead.extend_from_slice(&[0xC0, 20, 0, 1, 0, 1, 0, 0]);
        assert_eq!(answer(&ahead, PORT, host(), ADDR), None, "a pointer forwards");
        let mut reserved = vec![0u8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        reserved.extend_from_slice(&[0x40, 0, 0, 1, 0, 1]);
        assert_eq!(answer(&reserved, PORT, host(), ADDR), None, "a reserved label type");
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
