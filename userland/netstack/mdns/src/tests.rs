//! Every message here, sent or expected, is spelled from RFC 1035 §4.1's
//! layout and never by the responder's own writers; each test names the
//! section of RFC 6762 whose sentence it holds.

use super::*;
use std::vec;

const ADDR: [u8; 4] = [192, 168, 1, 49];
const MOVED: [u8; 4] = [192, 168, 1, 50];
const LINK: Link = Link { addr: ADDR, prefix: 24 };
const NEIGHBOUR: [u8; 4] = [192, 168, 1, 7];
/// Another responder on the link.
const PEER: Source = Source { addr: NEIGHBOUR, port: PORT };
const NAME: &[&str] = &["toyos-t14", "local"];
/// §18.2, §18.4: a response, authoritative.
const RESPONSE: u16 = 0x8400;

fn host() -> Host<'static> {
    Host::new("toyos-t14").expect("a label")
}

/// The draw of a call that starts no probing.
fn undrawn() -> u32 {
    panic!("no probing starts here, so no delay is drawn")
}

/// The probe for `addr`. §18.1, §18.2: ID zero and QR clear. §8.1: one
/// question, the name, type `ANY` (255), class 1 with the unicast-response
/// bit. §8.2: the proposed record in the Authority Section, which is the
/// header's third count: the name, type `A`, class 1 with no cache-flush bit
/// (§10.2 sets it in responses), §10's 120 s, four bytes of address.
fn probe_for(addr: [u8; 4]) -> Vec<u8> {
    let mut want = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0];
    want.extend_from_slice(b"\x09toyos-t14\x05local\x00\x00\xff\x80\x01");
    want.extend_from_slice(b"\x09toyos-t14\x05local\x00\x00\x01\x00\x01\x00\x00\x00\x78\x00\x04");
    want.extend_from_slice(&addr);
    want
}

/// The record for `addr` as a multicast carries it, asked for or announced:
/// §18's header (ID zero, `QR|AA`, no question), one `A` record with the
/// cache-flush bit (§10.2) and a 120 s TTL (§10).
fn record_of(addr: [u8; 4]) -> Vec<u8> {
    let mut want = vec![0, 0, 0x84, 0x00, 0, 0, 0, 1, 0, 0, 0, 0];
    want.extend_from_slice(b"\x09toyos-t14\x05local\x00");
    want.extend_from_slice(&[0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4]);
    want.extend_from_slice(&addr);
    want
}

fn put_name(out: &mut Vec<u8>, labels: &[&str]) {
    for label in labels {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

/// A record's name, type, class and data.
type Rr<'a> = (&'a [&'a str], u16, u16, &'a [u8]);

/// A message as RFC 1035 §4.1 lays one out: the header, the questions (name,
/// type, class), then the answer, authority and additional sections.
fn message(id: u16, flags: u16, questions: &[(&[&str], u16, u16)], sections: [&[Rr]; 3]) -> Vec<u8> {
    let mut m = Vec::new();
    for word in [id, flags, questions.len() as u16, sections[0].len() as u16, sections[1].len() as u16, sections[2].len() as u16] {
        m.extend_from_slice(&word.to_be_bytes());
    }
    for (name, kind, class) in questions {
        put_name(&mut m, name);
        m.extend_from_slice(&kind.to_be_bytes());
        m.extend_from_slice(&class.to_be_bytes());
    }
    for (name, kind, class, data) in sections.into_iter().flatten() {
        put_name(&mut m, name);
        m.extend_from_slice(&kind.to_be_bytes());
        m.extend_from_slice(&class.to_be_bytes());
        m.extend_from_slice(&120u32.to_be_bytes());
        m.extend_from_slice(&(data.len() as u16).to_be_bytes());
        m.extend_from_slice(data);
    }
    m
}

fn query(id: u16, name: &[&str], kind: u16, class: u16) -> Vec<u8> {
    message(id, 0, &[(name, kind, class)], [&[], &[], &[]])
}

/// Another host's response naming `addr` for this host's name.
fn says(addr: &[u8]) -> Vec<u8> {
    message(0, RESPONSE, &[], [&[(NAME, 1, 0x8001, addr)], &[], &[]])
}

/// Another host's probe for this host's name, proposing `proposed`; `class`
/// is its question's.
fn probe_of(class: u16, proposed: &[Rr]) -> Vec<u8> {
    message(0, 0, &[(NAME, 255, class)], [&[], proposed, &[]])
}

/// Every multicast `r` sends at its own deadlines up to `until`, each with
/// its time. Nothing is due a millisecond before a deadline, and `delay` is
/// drawn by the call that starts probing and by no other.
fn run(r: &mut Responder, link: Link, until: u64, delay: u32) -> Vec<(u64, Vec<u8>)> {
    let mut sent = Vec::new();
    for _ in 0..10_000 {
        let Some(at) = r.owed_at().filter(|at| *at <= until) else { return sent };
        let starts = matches!(r.claim, Claim::Owed { .. });
        if !starts && at > 0 {
            assert_eq!(r.on(Some(link), at - 1, undrawn), None, "nothing is due before {at}");
        }
        let mut drawn = false;
        let owed = r.on(Some(link), at, || {
            drawn = true;
            delay
        });
        assert_eq!(drawn, starts, "a delay is drawn by the call that starts probing, and by no other");
        sent.extend(owed.map(|bytes| (at, bytes)));
    }
    panic!("10,000 deadlines before {until}");
}

/// Three probes for `addr` from `first`, 250 ms apart, and the two
/// announcements from 250 ms after the third, a second apart.
fn claim_of(addr: [u8; 4], first: u64) -> Vec<(u64, Vec<u8>)> {
    let probes = [0, 250, 500].map(|after| (first + after, probe_for(addr)));
    let announcements = [750, 1_750].map(|after| (first + after, record_of(addr)));
    probes.into_iter().chain(announcements).collect()
}

/// A responder one probe into claiming `link`'s address: the probe left at
/// 1,000.
fn probing(link: Link) -> Responder<'static> {
    let mut r = Responder::new(host());
    assert_eq!(r.on(Some(link), 1_000, || 0), Some(probe_for(link.addr)));
    r
}

/// A responder that holds its name on [`LINK`], its second announcement out
/// at `at`.
fn held(at: u64) -> Responder<'static> {
    let mut r = Responder::new(host());
    assert_eq!(r.on(Some(LINK), at - 1_750, || 0), Some(probe_for(ADDR)));
    assert_eq!(run(&mut r, LINK, at, 0), claim_of(ADDR, at - 1_750)[1..]);
    assert_eq!((r.take_event(), r.owed_at()), (Some(Event::Claimed), None));
    r
}

/// One query from a neighbour on the link, long after the announcements.
fn ask(query: &[u8], port: u16) -> Option<Answer> {
    held(10_000).heard(query, Source { addr: NEIGHBOUR, port }, 100_000)
}

/// Whether `r` answers for its name at `now`, to a legacy resolver, which §6
/// never delays.
fn answers(r: &mut Responder, now: u64) -> bool {
    r.heard(&query(1, NAME, 1, 1), Source { addr: NEIGHBOUR, port: 53_000 }, now).is_some()
}

#[test]
fn a_query_for_this_name_is_answered_to_the_group() {
    let got = ask(&query(0, NAME, 1, 1), PORT).expect("an answer");
    assert_eq!(got, Answer { to: To::Group, bytes: record_of(ADDR) });
}

#[test]
fn a_name_is_matched_whatever_its_case_and_any_asks_for_it_too() {
    let q = query(0, &["ToyOS-T14", "LOCAL"], 255, 1);
    assert!(ask(&q, PORT).is_some());
}

#[test]
fn the_unicast_bit_sends_the_answer_back_to_the_asker() {
    let got = ask(&query(0, NAME, 1, 0x8001), PORT).expect("an answer");
    assert_eq!(got.to, To::Asker);
}

/// §6.7: a resolver that is not a responder gets its ID, its question and
/// a short TTL, and no cache-flush bit.
#[test]
fn a_legacy_resolver_gets_its_id_its_question_and_a_short_ttl() {
    let got = ask(&query(0xBEEF, NAME, 1, 1), 53_000).expect("an answer");
    assert_eq!(got.to, To::Asker);
    let mut want = vec![0xBE, 0xEF, 0x84, 0x00, 0, 1, 0, 1, 0, 0, 0, 0];
    want.extend_from_slice(b"\x09toyos-t14\x05local\x00\x00\x01\x00\x01");
    want.extend_from_slice(b"\x09toyos-t14\x05local\x00");
    want.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 10, 0, 4, 192, 168, 1, 49]);
    assert_eq!(got.bytes, want);
}

/// §18.3: "Multicast DNS messages received with an OPCODE other than zero
/// MUST be silently ignored." §18.11: "Multicast DNS messages received with
/// non-zero Response Codes MUST be silently ignored."
#[test]
fn a_question_that_is_not_this_hosts_is_not_answered() {
    for (name, kind, class) in [
        (&["other", "local"][..], 1, 1),
        (&["toyos-t14", "lan"][..], 1, 1),
        (&["toyos-t14"][..], 1, 1),
        (&["toyos-t14", "local", "x"][..], 1, 1),
        (NAME, 28, 1),
        (NAME, 1, 3),
    ] {
        assert_eq!(ask(&query(0, name, kind, class), PORT), None, "{name:?}");
    }
    for (flags, why) in [(0x0800, "a nonzero opcode"), (0x0003, "a nonzero response code")] {
        for port in [PORT, 53_000] {
            assert_eq!(ask(&message(0, flags, &[(NAME, 1, 1)], [&[], &[], &[]]), port), None, "{why}");
        }
    }
    assert_eq!(ask(&message(0, RESPONSE, &[(NAME, 1, 1)], [&[], &[], &[]]), PORT), None, "a response is not a question");
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
    let whole = query(0, NAME, 1, 1);
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
    let q = query(0, NAME, 1, 1);
    let mut r = Responder::new(host());
    assert_eq!(r.on(Some(LINK), 19_250, || 0), Some(probe_for(ADDR)));
    assert_eq!(run(&mut r, LINK, 20_000, 0), claim_of(ADDR, 19_250)[1..4], "claimed, and first announced at 20,000");
    assert_eq!(r.owed_at(), Some(21_000), "§8.3: the second announcement");
    assert_eq!(r.heard(&q, PEER, 20_500), None, "the record was announced 500 ms ago");
    let legacy = Source { addr: NEIGHBOUR, port: 53_000 };
    assert_eq!(r.heard(&q, legacy, 20_500).map(|a| a.to), Some(To::Asker), "a unicast answer is no multicast of the record");
    assert_eq!(r.on(Some(LINK), 20_999, undrawn), None);
    assert_eq!(r.on(Some(LINK), 21_000, undrawn), Some(record_of(ADDR)), "announced again, which answers the query");
    assert_eq!(r.owed_at(), None);

    assert_eq!(r.heard(&q, PEER, 21_400), None, "the record was multicast 400 ms ago");
    assert_eq!(r.heard(&q, PEER, 21_600), None);
    assert_eq!(r.owed_at(), Some(22_000), "both queries are owed one multicast when the second ends");
    assert_eq!(run(&mut r, LINK, 30_000, 0), [(22_000, record_of(ADDR))], "owed once");

    assert_eq!(r.heard(&q, PEER, 23_000).map(|a| a.to), Some(To::Group), "a second has passed");
    assert_eq!(r.heard(&q, PEER, 23_999), None);
    assert_eq!(r.owed_at(), Some(24_000));
}

/// RFC 6762 §8.1: "All probe queries SHOULD be done using the desired
/// resource record name and class (usually class 1, "Internet"), and query
/// type "ANY" (255)"; "the probes SHOULD be sent as "QU" questions with the
/// unicast-response bit set". §8.2: "each host populates the query message's
/// Authority Section with the record or records with the rdata that it would
/// be proposing to use, should its probing be successful."
#[test]
fn rfc6762_8_1_a_probe_asks_any_for_the_name_with_the_unicast_bit_and_proposes_its_record_in_the_authority_section() {
    let mut r = Responder::new(host());
    let probe = r.on(Some(LINK), 0, || 0).expect("no delay, so the first probe");
    #[rustfmt::skip]
    let want = [
        0, 0,  0, 0,  0, 1,  0, 0,  0, 1,  0, 0,
        9, b't', b'o', b'y', b'o', b's', b'-', b't', b'1', b'4', 5, b'l', b'o', b'c', b'a', b'l', 0,
        0, 255,  0x80, 1,
        9, b't', b'o', b'y', b'o', b's', b'-', b't', b'1', b'4', 5, b'l', b'o', b'c', b'a', b'l', 0,
        0, 1,  0, 1,  0, 0, 0, 120,  0, 4,  192, 168, 1, 49,
    ];
    assert_eq!(probe, want);
    assert_eq!(probe_for(ADDR), want, "which is what every other test expects of a probe");
    let sent = run(&mut r, LINK, 600, 0);
    assert_eq!(sent, [(250, want.to_vec()), (500, want.to_vec())], "each of the three is that message");
}

/// RFC 6762 §8.1: "the host should first wait for a short random delay time,
/// uniformly distributed in the range 0-250 ms." "250 ms after the first
/// query, the host should send a second; then, 250 ms after that, a third.
/// If, by 250 ms after the third probe, no conflicting Multicast DNS
/// responses have been received, the host may move to the next step,
/// announcing." §8.3: "at least two unsolicited responses, one second apart."
#[test]
fn rfc6762_8_1_three_probes_250_ms_apart_follow_the_drawn_delay_and_the_name_is_announced_250_ms_after_the_third() {
    for (draw, delay) in [(0, 0), (1, 1), (137, 137), (250, 250), (251, 0), (1_000, 247), (u32::MAX, u64::from(u32::MAX % 251))] {
        let mut r = Responder::new(host());
        let mut sent: Vec<_> = r.on(Some(LINK), 5_000, || draw).map(|probe| (5_000, probe)).into_iter().collect();
        assert_eq!(sent.is_empty(), delay != 0, "draw {draw}");
        assert_eq!(r.take_event(), None);
        sent.extend(run(&mut r, LINK, 5_000 + delay + 749, draw));
        assert_eq!(sent, claim_of(ADDR, 5_000 + delay)[..3], "draw {draw}: three probes, and nothing announced so far");
        assert_eq!(r.take_event(), None, "the name is not held 249 ms after the third probe");
        assert_eq!(run(&mut r, LINK, 60_000, draw), claim_of(ADDR, 5_000 + delay)[3..], "draw {draw}: announced twice, and nothing after");
        assert_eq!(r.take_event(), Some(Event::Claimed));
    }
}

/// RFC 6762 §8.3 announces "unique records that have completed the probing
/// step", and §6 has only "an authoritative source for a given record" answer
/// with it: under the probe nothing is answered, to anyone.
#[test]
fn a_name_under_probe_is_answered_to_nobody() {
    let mut r = Responder::new(host());
    assert_eq!(r.on(Some(LINK), 1_000, || 100), None);
    for now in [1_000, 1_099, 1_100, 1_350, 1_600, 1_849] {
        run(&mut r, LINK, now, 0);
        for (class, port) in [(1, PORT), (0x8001, PORT), (1, 53_000)] {
            let asked = r.heard(&query(7, NAME, 1, class), Source { addr: NEIGHBOUR, port }, now);
            assert_eq!(asked, None, "at {now}, class {class:#x} from port {port}");
        }
        assert_eq!(r.owed_at().map(|at| at > now), Some(true), "and no answer is held for later");
    }
    assert_eq!(run(&mut r, LINK, 1_850, 0), [(1_850, record_of(ADDR))]);
    assert!(answers(&mut r, 1_850), "held, and answered");
}

/// Whether a message heard from `from`, 100 ms after the first probe for
/// `link`'s address, made the responder defer for §8.2's second.
fn deferred(link: Link, from: [u8; 4], heard: &[u8]) -> bool {
    let mut r = probing(link);
    assert_eq!(r.heard(heard, Source { addr: from, port: PORT }, 1_100), None);
    assert_eq!(r.take_event(), None);
    match r.owed_at() {
        Some(1_250) => false,
        Some(2_100) => true,
        other => panic!("neither the second probe nor a second's wait: {other:?}"),
    }
}

/// RFC 6762 §8.2: "The two records are compared and the lexicographically
/// later data wins. This means that if the host finds that its own data is
/// lexicographically later, it simply ignores the other host's probe. If the
/// host finds that its own data is lexicographically earlier, then it defers
/// to the winning host by waiting one second, and then begins probing for
/// this record again." Its example: "MyPrinter.local. A 169.254.99.200" and
/// "MyPrinter.local. A 169.254.200.50": "169.254.200.50 is lexicographically
/// later (the third byte, with value 200, is greater than its counterpart
/// with value 99), so it is deemed the winner", which a signed comparison
/// gets wrong.
#[test]
fn rfc6762_8_2_the_lexicographically_later_data_wins_a_simultaneous_probe_and_the_earlier_waits_a_second() {
    let (early, late) = ([169, 254, 99, 200], [169, 254, 200, 50]);
    let on = |addr| Link { addr, prefix: 16 };
    let proposing = |addr: [u8; 4]| probe_of(0x8001, &[(NAME, 1, 1, &addr)]);
    assert!(deferred(on(early), late, &proposing(late)), "the earlier defers");
    assert!(!deferred(on(late), early, &proposing(early)), "the later ignores the other's probe");

    let mut r = probing(on(early));
    assert_eq!(r.heard(&proposing(late), Source { addr: late, port: PORT }, 1_100), None);
    assert_eq!(run(&mut r, on(early), 60_000, 0), claim_of(early, 2_100), "a second's wait, then the probing whole");
    assert_eq!(r.take_event(), Some(Event::Claimed), "nobody answered the second probing: a stale packet's name is claimed");

    let mut r = probing(on(late));
    assert_eq!(r.heard(&proposing(early), Source { addr: early, port: PORT }, 1_100), None);
    assert_eq!(run(&mut r, on(late), 60_000, 0), claim_of(late, 1_000)[1..], "the probing goes on as it was");
}

/// RFC 6762 §8.2: "first comparing the record class (excluding the
/// cache-flush bit described in Section 10.2), then the record type, then raw
/// comparison of the binary content of the rdata"; a record that "runs out of
/// rdata" first is the earlier. §8.2.1: the other host's records "are each
/// sorted into order, and then compared pairwise"; "If either list of records
/// runs out of records before any difference is found, then the list with
/// records remaining is deemed to have won the tiebreak. If both lists run
/// out of records at the same time without any difference being found, then
/// ... there is, in fact, no conflict." §8.2 consults "the Authority Section
/// of that query" for records "which answers the query".
#[test]
fn rfc6762_8_2_a_probe_is_compared_by_class_then_type_then_data_and_a_longer_list_wins() {
    let (lower, higher, longer) = ([192, 168, 1, 48], [192, 168, 1, 50], [192, 168, 1, 49, 0]);
    let cases: [(&str, &[Rr], bool); 15] = [
        ("later data", &[(NAME, 1, 1, &higher)], true),
        ("earlier data", &[(NAME, 1, 1, &lower)], false),
        ("the same record", &[(NAME, 1, 1, &ADDR)], false),
        ("a greater class with earlier data", &[(NAME, 1, 2, &lower)], true),
        ("a lesser class with later data", &[(NAME, 1, 0, &higher)], false),
        ("a greater class with a lesser type", &[(NAME, 0, 2, &lower)], true),
        ("the cache-flush bit is no part of the class", &[(NAME, 1, 0x8001, &lower)], false),
        ("a greater type with earlier data", &[(NAME, 28, 1, &[0; 16])], true),
        ("a lesser type with later data", &[(NAME, 0, 1, &higher)], false),
        ("data that runs out first", &[(NAME, 1, 1, &ADDR[..3])], false),
        ("data with more remaining", &[(NAME, 1, 1, &longer)], true),
        ("the same record and one more", &[(NAME, 28, 1, &[0; 16]), (NAME, 1, 1, &ADDR)], true),
        ("its earliest record is earlier", &[(NAME, 28, 1, &[0xff; 16]), (NAME, 1, 1, &lower)], false),
        ("another name's record", &[(&["other", "local"], 1, 1, &higher)], false),
        ("the same record in another case", &[(&["TOYOS-T14", "Local"], 1, 1, &higher)], true),
    ];
    for (what, theirs, loses) in cases {
        assert_eq!(deferred(LINK, NEIGHBOUR, &probe_of(1, theirs)), loses, "{what}");
    }
    let later: &[Rr] = &[(NAME, 1, 1, &higher)];
    let known = message(0, 0, &[(NAME, 255, 1)], [later, &[], &[]]);
    assert!(!deferred(LINK, NEIGHBOUR, &known), "a known answer proposes nothing");
    let elsewhere = message(0, 0, &[(&["other", "local"], 255, 1)], [&[], later, &[]]);
    assert!(!deferred(LINK, NEIGHBOUR, &elsewhere), "a probe for another name");
    let aaaa = message(0, 0, &[(NAME, 1, 1)], [&[], &[(NAME, 28, 1, &[0xff; 16])], &[]]);
    assert!(!deferred(LINK, NEIGHBOUR, &aaaa), "a record of another type answers no question for A");
    assert!(!deferred(LINK, [10, 0, 0, 7], &probe_of(1, later)), "§11: a probe from off the link");
    assert!(deferred(LINK, NEIGHBOUR, &probe_of(1, later)), "and the one all of those were not");
}

/// What a peer may send about the name, and whether it conflicts.
fn about_the_name() -> Vec<(&'static str, Vec<u8>, Source, bool)> {
    let other: &[Rr] = &[(NAME, 1, 0x8001, &MOVED)];
    let in_section = |at: usize| {
        let mut sections: [&[Rr]; 3] = [&[], &[], &[]];
        sections[at] = other;
        message(0, RESPONSE, &[], sections)
    };
    let mut by_pointer = message(0, RESPONSE, &[(NAME, 1, 1)], [&[], &[], &[]]);
    by_pointer[7] = 1;
    by_pointer.extend_from_slice(&[0xC0, 12, 0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4, 192, 168, 1, 50]);
    let mut counted_long = says(&MOVED);
    counted_long[6..12].copy_from_slice(&[0xff; 6]);
    let mut counted_alone = message(0, RESPONSE, &[], [&[], &[], &[]]);
    counted_alone[6..12].copy_from_slice(&[0xff; 6]);
    let mut overlong = says(&MOVED);
    let len = overlong.len();
    overlong[len - 5] = 5;
    vec![
        ("another address", says(&MOVED), PEER, true),
        ("the name in another case", message(0, RESPONSE, &[], [&[(&["ToyOS-T14", "LOCAL"], 1, 1, &[10, 0, 0, 1])], &[], &[]]), PEER, true),
        ("§8.1: a record with that name, of any type", message(0, RESPONSE, &[], [&[(NAME, 28, 1, &[0; 16])], &[], &[]]), PEER, true),
        ("another class", message(0, RESPONSE, &[], [&[(NAME, 1, 3, &ADDR)], &[], &[]]), PEER, true),
        ("no data", message(0, RESPONSE, &[], [&[(NAME, 1, 1, &[])], &[], &[]]), PEER, true),
        ("§9: in the answer section", in_section(0), PEER, true),
        ("§9: in the authority section", in_section(1), PEER, true),
        ("§9: in the additional section", in_section(2), PEER, true),
        ("behind another host's record", message(0, RESPONSE, &[], [&[(&["other", "local"], 1, 1, &MOVED), (NAME, 1, 1, &MOVED)], &[], &[]]), PEER, true),
        ("named by a pointer, behind a question §6 has ignored", by_pointer, PEER, true),
        ("with counts of records that are not there", counted_long, PEER, true),
        ("from a link-local source", says(&MOVED), Source { addr: [169, 254, 3, 4], port: PORT }, true),
        ("§9: identical rdata is never inconsistent", says(&ADDR), PEER, false),
        ("this record without the cache-flush bit", message(0, RESPONSE, &[], [&[(NAME, 1, 1, &ADDR)], &[], &[]]), PEER, false),
        ("another name's record", message(0, RESPONSE, &[], [&[(&["other", "local"], 1, 1, &MOVED)], &[], &[]]), PEER, false),
        ("a question and no record", message(0, RESPONSE, &[(NAME, 255, 1)], [&[], &[], &[]]), PEER, false),
        ("counts and no record", counted_alone, PEER, false),
        ("data longer than the message", overlong, PEER, false),
        ("§6: a response from a port other than 5353", says(&MOVED), Source { addr: NEIGHBOUR, port: 5_354 }, false),
        ("§11: a response from off the link", says(&MOVED), Source { addr: [10, 0, 0, 7], port: PORT }, false),
        ("§18.11: a response code", message(0, RESPONSE | 3, &[], [other, &[], &[]]), PEER, false),
        ("§18.3: an opcode", message(0, RESPONSE | 0x0800, &[], [other, &[], &[]]), PEER, false),
        ("a query's known answer", message(0, 0, &[], [other, &[], &[]]), PEER, false),
    ]
}

/// RFC 6762 §8.1: "During probing, from the time the first probe packet is
/// sent until 250 ms after the third probe, if any conflicting Multicast DNS
/// response is received, then the probing host MUST defer to the existing
/// host"; "any answer containing a record with that name, of any type, MUST
/// be considered a conflicting response". §9: "The protocol used in the
/// Probing phase will determine a winner and a loser, and the loser MUST
/// cease using the name".
#[test]
fn rfc6762_8_1_a_conflicting_response_under_the_probe_takes_the_name_and_nothing_else_does() {
    for (what, heard, from, conflicts) in about_the_name() {
        for (probes, now) in [(1, 1_001), (2, 1_300), (3, 1_500), (3, 1_749)] {
            let mut r = probing(LINK);
            assert_eq!(run(&mut r, LINK, now, 0).len(), probes - 1);
            assert_eq!(r.heard(&heard, from, now), None, "{what}");
            if !conflicts {
                assert_eq!(r.take_event(), None, "{what}, after {probes} probes");
                assert_eq!(run(&mut r, LINK, 60_000, 0), claim_of(ADDR, 1_000)[probes..], "{what}: the probing goes on");
                continue;
            }
            assert_eq!(r.take_event(), Some(Event::Lost(Lost::Answered)), "{what}, after {probes} probes");
            assert_eq!((r.take_event(), r.owed_at()), (None, None), "said once, and nothing is owed");
            assert_eq!(run(&mut r, LINK, 3_600_000, 0), [], "{what}: no probe and no announcement");
            assert_eq!(r.on(Some(LINK), 3_600_000, undrawn), None);
            assert!(!answers(&mut r, 3_600_000), "{what}: and no answer");
            for class in [1, 0x8001] {
                assert_eq!(r.heard(&query(0, NAME, 255, class), PEER, 3_600_000), None);
            }
        }
    }
}

/// RFC 6762 §8.1: "Apparently conflicting Multicast DNS responses received
/// *before* the first probe packet is sent MUST be silently ignored (see
/// discussion of stale probe packets in Section 8.2, "Simultaneous Probe
/// Tiebreaking", below)." A probe heard then is as stale.
#[test]
fn rfc6762_8_1_nothing_heard_before_the_first_probe_is_sent_is_a_conflict() {
    let later = probe_of(1, &[(NAME, 1, 1, &MOVED)]);
    let mut r = Responder::new(host());
    assert_eq!(r.on(Some(LINK), 1_000, || 200), None, "the delay");
    for heard in [says(&MOVED), later.clone()] {
        assert_eq!(r.heard(&heard, PEER, 1_100), None);
    }
    assert_eq!((r.take_event(), r.owed_at()), (None, Some(1_200)));
    assert_eq!(r.on(Some(LINK), 1_200, undrawn), Some(probe_for(ADDR)));

    assert_eq!(r.heard(&later, PEER, 1_300), None);
    assert_eq!(r.owed_at(), Some(2_300), "the first probe is out: a later probe is deferred to");
    for heard in [says(&MOVED), later.clone()] {
        assert_eq!(r.heard(&heard, PEER, 2_000), None);
    }
    assert_eq!((r.take_event(), r.owed_at()), (None, Some(2_300)), "and in the second's wait no probe of the new probing is out");
    assert_eq!(run(&mut r, LINK, 60_000, 0), claim_of(ADDR, 2_300));
}

/// RFC 6762 §9: "Whenever a Multicast DNS responder receives any Multicast
/// DNS response (solicited or otherwise) containing a conflicting resource
/// record in any of the Resource Record Sections, the Multicast DNS responder
/// MUST immediately reset its conflicted unique record to probing state, and
/// go through the startup steps described above in Section 8".
#[test]
fn rfc6762_9_a_conflicting_response_resets_a_held_name_to_probing_and_nothing_else_does() {
    for (what, heard, from, conflicts) in about_the_name() {
        let mut r = held(10_000);
        assert_eq!(r.heard(&heard, from, 50_000), None, "{what}");
        assert_eq!(r.take_event(), None, "{what}: the name is neither lost nor claimed yet");
        if !conflicts {
            assert_eq!(r.owed_at(), None, "{what}");
            assert!(answers(&mut r, 50_000), "{what}: still held");
            continue;
        }
        assert_eq!(r.owed_at(), Some(50_000), "{what}: probing is owed at once");
        assert!(!answers(&mut r, 50_000), "{what}: and the name is answered to nobody");
        assert_eq!(r.heard(&query(0, NAME, 1, 1), PEER, 50_000), None);
        assert_eq!(run(&mut r, LINK, 50_326, 77), claim_of(ADDR, 50_077)[..1], "{what}: probed after a drawn delay");
        assert!(!answers(&mut r, 50_326));
        assert_eq!(run(&mut r, LINK, 3_600_000, 77), claim_of(ADDR, 50_077)[1..], "{what}: unanswered, the name is announced again");
        assert_eq!(r.take_event(), Some(Event::Claimed));
        assert!(answers(&mut r, 60_000));
    }
}

/// RFC 6762 §9: "The protocol used in the Probing phase will determine a
/// winner and a loser, and the loser MUST cease using the name". A host that
/// holds the name answers the probe §9 asks for, and that takes it (§8.1).
#[test]
fn rfc6762_9_a_held_name_another_host_answers_for_under_the_new_probe_is_lost() {
    let mut r = held(10_000);
    assert_eq!(r.heard(&says(&MOVED), PEER, 50_000), None);
    assert_eq!(run(&mut r, LINK, 50_100, 0), [(50_000, probe_for(ADDR))]);
    assert_eq!(r.heard(&says(&MOVED), PEER, 50_100), None, "the holder defends");
    assert_eq!((r.take_event(), r.owed_at()), (Some(Event::Lost(Lost::Answered)), None));
    assert_eq!(run(&mut r, LINK, 3_600_000, 0), []);
    assert!(!answers(&mut r, 3_600_000));
}

/// RFC 6762 §6: "In the special case of answering probe queries, because of
/// the limited time before the probing host will make its decision about
/// whether or not to use the name, a Multicast DNS responder MUST respond
/// quickly. In this special case only, when responding via multicast to a
/// probe, a Multicast DNS responder is only required to delay its
/// transmission as necessary to ensure an interval of at least 250 ms since
/// the last time the record was multicast on that interface." "A probe query
/// can be distinguished from a normal query by the fact that a probe query
/// contains a proposed record in the Authority Section that answers the
/// question". §8.1 has probes sent "QU" "to allow a defending host to respond
/// immediately via unicast".
#[test]
fn rfc6762_6_a_probe_for_a_held_name_is_answered_at_once_by_unicast_or_250_ms_after_the_records_last_multicast() {
    let theirs: &[Rr] = &[(NAME, 1, 1, &NEIGHBOUR)];
    let (qu, qm, plain) = (probe_of(0x8001, theirs), probe_of(1, theirs), query(0, NAME, 255, 1));

    let mut r = held(10_000);
    let defended = r.heard(&qu, PEER, 10_001);
    assert_eq!(defended, Some(Answer { to: To::Asker, bytes: record_of(ADDR) }), "a QU probe, a millisecond after an announcement");

    assert_eq!(r.heard(&qm, PEER, 10_100), None, "a QM probe 100 ms after the record's last multicast");
    assert_eq!(r.owed_at(), Some(10_250), "is answered at 250 ms, inside the 750 ms its sender waits");
    assert_eq!(run(&mut r, LINK, 20_000, 0), [(10_250, record_of(ADDR))]);

    let mut r = held(10_000);
    assert_eq!(r.heard(&plain, PEER, 10_100), None, "a query that is no probe");
    assert_eq!(r.owed_at(), Some(11_000), "waits out the second");
    assert_eq!(r.heard(&qm, PEER, 10_200), None);
    assert_eq!(r.owed_at(), Some(10_250), "and a probe behind it is not held to the query's second");
    assert_eq!(r.heard(&plain, PEER, 10_240), None);
    assert_eq!(run(&mut r, LINK, 20_000, 0), [(10_250, record_of(ADDR))], "one multicast answers all three");

    let mut r = held(10_000);
    assert_eq!(r.heard(&qm, PEER, 10_250).map(|a| a.to), Some(To::Group), "250 ms have passed");
    assert_eq!(r.heard(&qm, PEER, 10_499), None);
    assert_eq!(r.owed_at(), Some(10_500));
    let known = message(0, 0, &[(NAME, 255, 1)], [theirs, &[], &[]]);
    let mut r = held(10_000);
    assert_eq!(r.heard(&known, PEER, 10_300), None, "a known answer is no proposed record");
    assert_eq!(r.owed_at(), Some(11_000));
}

/// `count` returns of the link `every` ms apart from 10,000, each probed on
/// at once and answered for by another host a millisecond later. Returns the
/// responder and when the last conflict was.
fn answered_on_every_return(count: u64, every: u64) -> (Responder<'static>, u64) {
    let mut r = held(5_000);
    let mut last = 0;
    for i in 0..count {
        let now = 10_000 + i * every;
        r.link_returned(now);
        assert_eq!(r.on(Some(LINK), now, || 0), Some(probe_for(ADDR)), "return {i}: not limited yet");
        assert_eq!(r.heard(&says(&MOVED), PEER, now + 1), None);
        assert_eq!(r.take_event(), Some(Event::Lost(Lost::Answered)));
        last = now + 1;
    }
    (r, last)
}

/// RFC 6762 §8.1: "If fifteen conflicts occur within any ten-second period,
/// then the host MUST wait at least five seconds before each successive
/// additional probe attempt."
#[test]
fn rfc6762_8_1_fifteen_conflicts_in_ten_seconds_put_five_seconds_before_each_further_probe_attempt() {
    // 14 x 714 ms is 9,996 ms: the fifteen fall within ten seconds.
    let (mut r, last) = answered_on_every_return(15, 714);
    let now = last + 700;
    r.link_returned(now);
    assert_eq!(r.on(Some(LINK), now, || 30), None);
    assert_eq!(r.owed_at(), Some(now + 5_030), "five seconds, and the drawn delay");
    assert_eq!(run(&mut r, LINK, now + 5_100, 30), [(now + 5_030, probe_for(ADDR))]);

    assert_eq!(r.heard(&probe_of(1, &[(NAME, 1, 1, &MOVED)]), PEER, now + 5_100), None, "a later probe");
    assert_eq!(r.owed_at(), Some(now + 5_100 + 6_000), "§8.2's second, and the five");
    assert_eq!(run(&mut r, LINK, now + 11_200, 30), [(now + 11_100, probe_for(ADDR))]);
    assert_eq!(r.heard(&says(&MOVED), PEER, now + 11_200), None);
    assert_eq!(r.take_event(), Some(Event::Lost(Lost::Answered)));

    // The newest fifteen now span more than ten seconds, and each still came
    // within ten of the one before: "each successive additional probe attempt".
    r.link_returned(now + 12_000);
    assert_eq!(r.on(Some(LINK), now + 12_000, || 0), None);
    assert_eq!(r.owed_at(), Some(now + 17_000));

    // Ten seconds with no conflict end it.
    r.link_returned(now + 21_201);
    assert_eq!(r.on(Some(LINK), now + 21_201, || 0), Some(probe_for(ADDR)));
    assert_eq!(run(&mut r, LINK, 3_600_000, 0), claim_of(ADDR, now + 21_201)[1..]);
}

/// RFC 6762 §8.1's limit is of "fifteen conflicts" "within any ten-second
/// period": fourteen are not fifteen, and fifteen in 10.01 s are not within
/// ten.
#[test]
fn rfc6762_8_1_fourteen_conflicts_or_fifteen_in_more_than_ten_seconds_limit_nothing() {
    for (count, every) in [(14, 100), (15, 715), (40, 750)] {
        let (mut r, last) = answered_on_every_return(count, every);
        r.link_returned(last + 700);
        assert_eq!(r.on(Some(LINK), last + 700, || 0), Some(probe_for(ADDR)), "{count} conflicts {every} ms apart");
    }
}

/// RFC 6762 §8: "Whenever a Multicast DNS responder starts up, wakes up from
/// sleep, receives an indication of a network interface "Link Change" event,
/// or has any other reason to believe that its network connectivity may have
/// changed in some relevant way, it MUST perform the two startup steps below:
/// Probing (Section 8.1) and Announcing (Section 8.3)." §6 still holds: the
/// record a query had multicast inside the last second is first announced
/// when that second ends.
#[test]
fn rfc6762_8_a_link_that_returns_is_probed_on_before_the_name_is_announced_or_answered_with_again() {
    let mut r = held(10_000);
    assert_eq!((r.on(Some(LINK), 60_000, undrawn), r.owed_at()), (None, None), "nothing is owed a link that stayed");

    r.link_returned(60_000);
    assert_eq!(r.owed_at(), Some(60_000));
    assert!(!answers(&mut r, 60_000), "not this host's until probed for again");
    assert_eq!(run(&mut r, LINK, 3_600_000, 120), claim_of(ADDR, 60_120));
    assert_eq!(r.take_event(), Some(Event::Claimed));

    assert_eq!(r.heard(&query(0, NAME, 1, 1), PEER, 99_900).map(|a| a.to), Some(To::Group));
    r.link_returned(100_000);
    let sent = run(&mut r, LINK, 3_600_000, 0);
    assert_eq!(sent[..3], claim_of(ADDR, 100_000)[..3]);
    assert_eq!(sent[3..], [(100_900, record_of(ADDR)), (101_900, record_of(ADDR))], "§6: a second after the answer at 99,900");

    let mut lost = probing(LINK);
    assert_eq!(lost.heard(&says(&MOVED), PEER, 1_100), None);
    assert_eq!(lost.take_event(), Some(Event::Lost(Lost::Answered)));
    lost.link_returned(9_000);
    assert_eq!(run(&mut lost, LINK, 3_600_000, 5), claim_of(ADDR, 9_005), "a lost name is probed for on the link's return");
    assert_eq!(lost.take_event(), Some(Event::Claimed));

    let mut unaddressed = Responder::new(host());
    unaddressed.link_returned(5_000);
    assert_eq!(unaddressed.owed_at(), None, "no address, no record to probe for");
    assert_eq!(unaddressed.on(Some(LINK), 9_000, || 0), Some(probe_for(ADDR)), "the address that comes is probed on once");
    assert_eq!(run(&mut unaddressed, LINK, 3_600_000, 0), claim_of(ADDR, 9_000)[1..]);
}

/// A link that returns again under the probe starts it over: each return is
/// a reason of its own to doubt the name (§8), nothing is announced until one
/// probing has run whole, and a return costs no more than its three probes.
#[test]
fn a_flapping_link_restarts_the_probe_and_nothing_is_announced_until_one_runs_whole() {
    let mut r = held(10_000);
    let mut sent = Vec::new();
    for flap in 0..50 {
        let now = 20_000 + flap * 100;
        r.link_returned(now);
        sent.extend(run(&mut r, LINK, now + 99, 150));
        assert!(!answers(&mut r, now + 99));
    }
    assert_eq!(sent, [], "a return inside the last one's delay sends nothing");
    assert_eq!(run(&mut r, LINK, 3_600_000, 150), claim_of(ADDR, 24_900 + 150));
    assert_eq!(r.take_event(), Some(Event::Claimed));

    let mut sent = Vec::new();
    for flap in 0..50 {
        let now = 40_000 + flap * 600;
        r.link_returned(now);
        sent.extend(run(&mut r, LINK, now + 599, 0));
        assert!(!answers(&mut r, now + 599));
    }
    let probes: Vec<_> = (0..50).flat_map(|flap| claim_of(ADDR, 40_000 + flap * 600).into_iter().take(3)).collect();
    assert_eq!(sent, probes, "three probes a return, and no announcement between two returns 600 ms apart");
    assert_eq!(r.take_event(), None);
    assert_eq!(run(&mut r, LINK, 3_600_000, 0), claim_of(ADDR, 49 * 600 + 40_000)[3..]);
    assert_eq!(r.take_event(), Some(Event::Claimed));
}

/// A storm of forged answers, one a millisecond for ten seconds, at a held
/// name: the first resets it to probing (§9), the next after its first probe
/// takes it (§8.1), and the other 9,998 are read by a responder that holds no
/// name and sends nothing.
#[test]
fn a_storm_of_forged_answers_costs_one_probe_and_the_name() {
    let mut r = held(10_000);
    let mut sent = Vec::new();
    for ms in 0..10_000 {
        sent.extend(run(&mut r, LINK, 50_000 + ms, 0));
        assert_eq!(r.heard(&says(&MOVED), PEER, 50_000 + ms), None);
    }
    assert_eq!(sent, [(50_000, probe_for(ADDR))]);
    assert_eq!((r.take_event(), r.owed_at()), (Some(Event::Lost(Lost::Answered)), None));
    assert_eq!(run(&mut r, LINK, 3_600_000, 0), []);
}

/// Forged answers timed to spare every probe, one before each second
/// announcement of a name held again: each costs a probing (§9), and the
/// fifteenth since the link last returned costs the name, so the forger's
/// packets buy fourteen probings and no more, however long it goes on. A
/// return of the link is no peer's packet, and the count starts over at it.
#[test]
fn forged_answers_that_spare_every_probe_cost_fourteen_probings_and_then_the_name() {
    let mut r = held(10_000);
    let (mut now, mut sent, mut forged) = (50_000, Vec::new(), 0);
    while !matches!(r.claim, Claim::Lost) {
        assert_eq!(r.heard(&says(&MOVED), PEER, now), None);
        forged += 1;
        let Some(at) = r.owed_at() else { break };
        // The name is held again and announced once; the forger strikes before the second.
        let round = run(&mut r, LINK, at + 1_749, 0);
        assert_eq!(round, claim_of(ADDR, at)[..4], "forged answer {forged}");
        assert_eq!(r.take_event(), Some(Event::Claimed));
        sent.extend(round);
        now = at + 1_749;
    }
    assert_eq!((forged, sent.len()), (CONFLICTS, 14 * 4), "fourteen probings: three probes and an announcement each");
    assert_eq!((r.take_event(), r.owed_at()), (Some(Event::Lost(Lost::Contested)), None));
    for later in 0..1_000 {
        assert_eq!(r.heard(&says(&MOVED), PEER, now + later), None);
    }
    assert_eq!(run(&mut r, LINK, 3_600_000, 0), [], "and nothing more, whatever it sends");

    r.link_returned(4_000_000);
    assert_eq!(run(&mut r, LINK, 5_000_000, 0), claim_of(ADDR, 4_000_000));
    for forged in 0..14 {
        let now = 5_000_000 + forged * 20_000;
        assert_eq!(r.heard(&says(&MOVED), PEER, now), None);
        assert_eq!(run(&mut r, LINK, now + 19_999, 0), claim_of(ADDR, now), "fourteen more are fourteen since the link returned");
    }
    r.link_returned(6_000_000);
    assert_eq!(run(&mut r, LINK, 6_100_000, 0), claim_of(ADDR, 6_000_000));
    assert_eq!(r.heard(&says(&MOVED), PEER, 6_100_000), None);
    assert_eq!(run(&mut r, LINK, 6_200_000, 0), claim_of(ADDR, 6_100_000), "and the fifteenth is the first since the next return");
}

/// Forged probes that win every tiebreak, one behind each first probe: each
/// costs a second's wait and one probe (§8.2), and the fifteenth costs the
/// name. A probe a second never reaches §8.1's fifteen in ten seconds.
#[test]
fn forged_probes_that_win_every_tiebreak_cost_fifteen_probes_and_then_the_name() {
    let later = probe_of(1, &[(NAME, 1, 1, &MOVED)]);
    let mut r = probing(LINK);
    let mut sent = vec![1_000];
    for forged in 1..CONFLICTS as u64 {
        assert_eq!(r.heard(&later, PEER, 1_000 * forged + 1), None);
        assert_eq!(r.take_event(), None);
        let probe = run(&mut r, LINK, 1_000 * forged + 1_100, 0);
        assert_eq!(probe, [(1_000 * forged + 1_001, probe_for(ADDR))], "a second after forged probe {forged}");
        sent.push(probe[0].0);
    }
    assert_eq!(r.heard(&later, PEER, 16_000), None);
    assert_eq!((r.take_event(), r.owed_at()), (Some(Event::Lost(Lost::Contested)), None));
    assert_eq!(sent.len(), CONFLICTS);
    assert_eq!(run(&mut r, LINK, 3_600_000, 0), []);
}

/// Nothing a peer sends is trusted to be a message: no cut of a conflicting
/// response or of a winning probe is either, under the probe or at a held
/// name, and the whole one behind them still is.
#[test]
fn no_cut_of_a_conflicting_response_or_of_a_winning_probe_is_one() {
    let (response, later) = (says(&MOVED), probe_of(1, &[(NAME, 1, 1, &MOVED)]));
    for cut in 0..response.len() {
        let mut r = probing(LINK);
        assert_eq!(r.heard(&response[..cut], PEER, 1_100), None);
        assert_eq!((r.take_event(), r.owed_at()), (None, Some(1_250)), "under the probe, cut at {cut}");
        let mut r = held(10_000);
        assert_eq!(r.heard(&response[..cut], PEER, 50_000), None);
        assert_eq!((r.take_event(), r.owed_at()), (None, None), "held, cut at {cut}");
    }
    for cut in 0..later.len() {
        let mut r = probing(LINK);
        assert_eq!(r.heard(&later[..cut], PEER, 1_100), None);
        assert_eq!((r.take_event(), r.owed_at()), (None, Some(1_250)), "cut at {cut}");
    }
    let mut r = probing(LINK);
    assert_eq!(r.heard(&later, PEER, 1_100), None);
    assert_eq!(r.owed_at(), Some(2_100));
    assert_eq!(r.heard(&response, PEER, 2_100), None, "in the wait");
    assert_eq!(run(&mut r, LINK, 2_100, 0), [(2_100, probe_for(ADDR))]);
    assert_eq!(r.heard(&response, PEER, 2_101), None);
    assert_eq!(r.take_event(), Some(Event::Lost(Lost::Answered)));
}

/// RFC 6762 §8.4: "if any of a host's IP addresses change, it MUST
/// re-announce those address records. The host does not need to repeat the
/// Probing step because it has already established unique ownership of that
/// name." §10.2: a host that "has not been continuously connected and
/// participating on the network link" "MUST first probe": an address after
/// none is probed on. Nothing is answered without an address.
#[test]
fn an_address_after_none_is_probed_for_and_a_new_one_under_a_held_name_is_announced() {
    let mut r = Responder::new(host());
    assert!(!answers(&mut r, 0), "no address yet");
    assert_eq!(r.on(None, 0, undrawn), None);
    assert_eq!(r.owed_at(), None);
    assert_eq!(r.on(Some(LINK), 0, || 0), Some(probe_for(ADDR)));
    assert_eq!(run(&mut r, LINK, 5_000, 0), claim_of(ADDR, 0)[1..]);
    assert_eq!(r.take_event(), Some(Event::Claimed));

    let moved = Link { addr: MOVED, prefix: 24 };
    assert_eq!(r.on(Some(moved), 9_000, undrawn), Some(record_of(MOVED)), "announced at once, and not probed for");
    assert_eq!(run(&mut r, moved, 60_000, 0), [(10_000, record_of(MOVED))], "§8.3: and a second later");
    assert_eq!(r.take_event(), None);

    assert_eq!(r.on(Some(moved), 60_500, undrawn), None);
    assert_eq!(r.on(None, 60_600, undrawn), None);
    assert_eq!(r.owed_at(), None, "nothing is owed for an address no longer held");
    assert!(!answers(&mut r, 60_600), "no address any more");
    assert_eq!(r.on(Some(LINK), 60_700, || 9), None);
    assert_eq!(run(&mut r, LINK, 3_600_000, 9), claim_of(ADDR, 60_709), "an address after none is probed on");

    let mut r = probing(LINK);
    assert_eq!(r.on(Some(moved), 1_250, undrawn), Some(probe_for(MOVED)), "a probe proposes the address held as it leaves");
    assert_eq!(r.heard(&says(&ADDR), PEER, 1_300), None, "and the address given up is another's");
    assert_eq!(r.take_event(), Some(Event::Lost(Lost::Answered)));
    assert_eq!((r.on(Some(LINK), 2_000, undrawn), r.owed_at()), (None, None), "a lost name is not claimed by a new address");
    assert_eq!(r.on(None, 3_000, undrawn), None);
    assert_eq!(r.on(Some(LINK), 4_000, || 0), Some(probe_for(ADDR)), "but by one after none");
}

/// RFC 6762 §11: a query whose source is not on this link is ignored; a
/// link-local source (RFC 3927) is on every link. RFC 1122 §3.2.1.3: a
/// loopback source is never on a wire, and a datagram carrying one is
/// silently discarded.
#[test]
fn a_query_from_off_the_link_is_not_answered() {
    let q = query(0, NAME, 1, 1);
    for addr in [[10, 0, 0, 7], [192, 168, 2, 7], [8, 8, 8, 8], [127, 0, 0, 1], [127, 1, 2, 3]] {
        for port in [PORT, 53_000] {
            assert_eq!(held(10_000).heard(&q, Source { addr, port }, 100_000), None, "{addr:?}:{port}");
        }
    }
    for addr in [[192, 168, 1, 254], [169, 254, 3, 4]] {
        let asked = held(10_000).heard(&q, Source { addr, port: PORT }, 100_000);
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
