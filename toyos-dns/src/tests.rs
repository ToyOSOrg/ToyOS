//! Each test names the RFC section it holds the reader to. The four `REAL_*`
//! replies are the independent half: bytes Cloudflare's public resolver sent
//! to this crate's own queries, compression and all.

use super::*;
use std::vec;
use std::vec::Vec;

fn name(text: &str) -> Name {
    Name::parse(text).unwrap()
}

/// `www.apple.com`, ID 0x1000: three aliases, each target compressed by a
/// pointer into the data of the record before it, then one `A`.
const REAL_APPLE: [u8; 161] = [
    0x10, 0x00, 0x81, 0x80, 0x00, 0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x03, 0x77, 0x77, 0x77, 0x05, 0x61,
    0x70, 0x70, 0x6c, 0x65, 0x03, 0x63, 0x6f, 0x6d, 0x00, 0x00, 0x01, 0x00, 0x01, 0xc0, 0x0c, 0x00, 0x05, 0x00,
    0x01, 0x00, 0x00, 0x01, 0x21, 0x00, 0x1a, 0x0d, 0x77, 0x77, 0x77, 0x2d, 0x61, 0x70, 0x70, 0x6c, 0x65, 0x2d,
    0x63, 0x6f, 0x6d, 0x01, 0x76, 0x07, 0x61, 0x61, 0x70, 0x6c, 0x69, 0x6d, 0x67, 0xc0, 0x16, 0xc0, 0x2b, 0x00,
    0x05, 0x00, 0x01, 0x00, 0x00, 0x01, 0x21, 0x00, 0x1b, 0x03, 0x77, 0x77, 0x77, 0x05, 0x61, 0x70, 0x70, 0x6c,
    0x65, 0x03, 0x63, 0x6f, 0x6d, 0x07, 0x65, 0x64, 0x67, 0x65, 0x6b, 0x65, 0x79, 0x03, 0x6e, 0x65, 0x74, 0x00,
    0xc0, 0x51, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x01, 0x21, 0x00, 0x19, 0x05, 0x65, 0x36, 0x38, 0x35, 0x38,
    0x05, 0x64, 0x73, 0x63, 0x65, 0x39, 0x0a, 0x61, 0x6b, 0x61, 0x6d, 0x61, 0x69, 0x65, 0x64, 0x67, 0x65, 0xc0,
    0x67, 0xc0, 0x78, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x09, 0x00, 0x04, 0x17, 0x20, 0x70, 0xf6,
];

/// `dns.google`, ID 0x1001: two `A` records, owners compressed to the
/// question.
const REAL_GOOGLE: [u8; 60] = [
    0x10, 0x01, 0x81, 0x80, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x03, 0x64, 0x6e, 0x73, 0x06, 0x67,
    0x6f, 0x6f, 0x67, 0x6c, 0x65, 0x00, 0x00, 0x01, 0x00, 0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
    0x02, 0xcf, 0x00, 0x04, 0x08, 0x08, 0x04, 0x04, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0xcf,
    0x00, 0x04, 0x08, 0x08, 0x08, 0x08,
];

/// `doesnotexist.invalid`, ID 0x1002: NXDOMAIN, with the root's SOA in the
/// authority section (RFC 6761 §6.4 reserves `.invalid`).
const REAL_INVALID: [u8; 113] = [
    0x10, 0x02, 0x81, 0x83, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x0c, 0x64, 0x6f, 0x65, 0x73, 0x6e,
    0x6f, 0x74, 0x65, 0x78, 0x69, 0x73, 0x74, 0x07, 0x69, 0x6e, 0x76, 0x61, 0x6c, 0x69, 0x64, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0x06, 0x00, 0x01, 0x00, 0x01, 0x51, 0x80, 0x00, 0x40, 0x01, 0x61, 0x0c, 0x72, 0x6f,
    0x6f, 0x74, 0x2d, 0x73, 0x65, 0x72, 0x76, 0x65, 0x72, 0x73, 0x03, 0x6e, 0x65, 0x74, 0x00, 0x05, 0x6e, 0x73,
    0x74, 0x6c, 0x64, 0x0c, 0x76, 0x65, 0x72, 0x69, 0x73, 0x69, 0x67, 0x6e, 0x2d, 0x67, 0x72, 0x73, 0x03, 0x63,
    0x6f, 0x6d, 0x00, 0x78, 0xc3, 0xb7, 0xd6, 0x00, 0x00, 0x07, 0x08, 0x00, 0x00, 0x03, 0x84, 0x00, 0x09, 0x3a,
    0x80, 0x00, 0x01, 0x51, 0x80,
];

/// `www.microsoft.com`, ID 0x1003: two aliases, then one `A`.
const REAL_MICROSOFT: [u8; 135] = [
    0x10, 0x03, 0x81, 0x80, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x03, 0x77, 0x77, 0x77, 0x09, 0x6d,
    0x69, 0x63, 0x72, 0x6f, 0x73, 0x6f, 0x66, 0x74, 0x03, 0x63, 0x6f, 0x6d, 0x00, 0x00, 0x01, 0x00, 0x01, 0xc0,
    0x0c, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00, 0x0e, 0x03, 0x00, 0x23, 0x03, 0x77, 0x77, 0x77, 0x09, 0x6d, 0x69,
    0x63, 0x72, 0x6f, 0x73, 0x6f, 0x66, 0x74, 0x07, 0x63, 0x6f, 0x6d, 0x2d, 0x63, 0x2d, 0x33, 0x07, 0x65, 0x64,
    0x67, 0x65, 0x6b, 0x65, 0x79, 0x03, 0x6e, 0x65, 0x74, 0x00, 0xc0, 0x2f, 0x00, 0x05, 0x00, 0x01, 0x00, 0x00,
    0x03, 0x77, 0x00, 0x19, 0x06, 0x65, 0x31, 0x33, 0x36, 0x37, 0x38, 0x04, 0x64, 0x73, 0x63, 0x62, 0x0a, 0x61,
    0x6b, 0x61, 0x6d, 0x61, 0x69, 0x65, 0x64, 0x67, 0x65, 0xc0, 0x4d, 0xc0, 0x5e, 0x00, 0x01, 0x00, 0x01, 0x00,
    0x00, 0x00, 0x07, 0x00, 0x04, 0x17, 0xd4, 0xc1, 0xda,
];

/// Every real reply, with the ID and name it answers.
fn real() -> [(&'static [u8], u16, Name); 4] {
    [
        (&REAL_APPLE, 0x1000, name("www.apple.com")),
        (&REAL_GOOGLE, 0x1001, name("dns.google")),
        (&REAL_INVALID, 0x1002, name("doesnotexist.invalid")),
        (&REAL_MICROSOFT, 0x1003, name("www.microsoft.com")),
    ]
}

// --- A message builder, for the replies no resolver would send ---

const OK: u16 = QR | RD | 0x0080;

fn wire(text: &str) -> Vec<u8> {
    name(text).wire().to_vec()
}

fn rr(owner: &[u8], rtype: u16, class: u16, data: &[u8]) -> Vec<u8> {
    let mut out = owner.to_vec();
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&300u32.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
    out
}

fn a(owner: &str, addr: [u8; 4]) -> Vec<u8> {
    rr(&wire(owner), TYPE_A, CLASS_IN, &addr)
}

fn cname(owner: &str, target: &str) -> Vec<u8> {
    rr(&wire(owner), TYPE_CNAME, CLASS_IN, &wire(target))
}

/// A response with ID `id`, `flags`, one question for `asked`'s `A`, and
/// `answers` as the answer section.
fn reply(id: u16, flags: u16, asked: &str, answers: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for word in [id, flags, 1, answers.len() as u16, 0, 0] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out.extend_from_slice(&wire(asked));
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    for answer in answers {
        out.extend_from_slice(answer);
    }
    out
}

fn answer(aliases: &[&str], addrs: &[[u8; 4]]) -> Verdict {
    Verdict::Answer(Answer { aliases: aliases.iter().map(|a| name(a)).collect(), addrs: addrs.to_vec() })
}

fn stray(why: &'static str) -> Verdict {
    Verdict::Stray(Stray::Malformed(why))
}

// --- The question ---

#[test]
fn rfc1035_4_1_1_a_query_is_one_header_and_one_question_for_a_in_in() {
    assert_eq!(
        query(0xbeef, &name("dns.google")),
        [
            0xbe, 0xef, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, // ID, RD, QDCOUNT 1
            3, b'd', b'n', b's', 6, b'g', b'o', b'o', b'g', b'l', b'e', 0, // QNAME
            0, 1, 0, 1, // QTYPE A, QCLASS IN
        ]
    );
    assert_eq!(query(1, &name("dns.google.")), query(1, &name("dns.google")), "the root's dot is the same name");
}

#[test]
fn rfc1035_2_3_4_a_label_is_63_bytes_and_a_name_255() {
    let label = "a".repeat(63);
    assert!(Name::parse(&label).is_ok());
    assert_eq!(Name::parse(&"a".repeat(64)), Err(BadName::LongLabel));
    // 63+1 bytes a label, four labels, and the root: 257 bytes. Three labels
    // and one of 61 is exactly 255.
    let at_limit = [label.as_str(), &label, &label, &"b".repeat(61)].join(".");
    assert_eq!(Name::parse(&at_limit).unwrap().wire().len(), 255);
    let past = [label.as_str(), &label, &label, &"b".repeat(62)].join(".");
    assert_eq!(Name::parse(&past), Err(BadName::TooLong));
}

#[test]
fn rfc1123_2_1_a_name_is_letters_digits_hyphens_and_nothing_else() {
    assert!(Name::parse("xn--bcher-kva.example").is_ok());
    assert!(Name::parse("_dmarc.example").is_ok());
    for (text, refused) in [
        ("", BadName::Empty),
        (".", BadName::Empty),
        ("a..b", BadName::EmptyLabel),
        (".a", BadName::EmptyLabel),
        ("a b", BadName::Character(b' ')),
        ("a\0b", BadName::Character(0)),
        ("bücher.example", BadName::Character(0xc3)),
        ("a\\.b", BadName::Character(b'\\')),
    ] {
        assert_eq!(Name::parse(text), Err(refused), "{text:?}");
    }
}

#[test]
fn rfc4343_names_compare_without_regard_to_case() {
    assert!(name("WWW.Example.COM").same(&name("www.example.com")));
    assert!(!name("www.example.com").same(&name("www.example.co")));
    assert!(!name("a.bc").same(&name("ab.c")), "the label boundaries are part of the name");
}

#[test]
fn rfc1035_5_1_a_name_off_the_wire_is_written_with_every_odd_byte_escaped() {
    use std::string::ToString;
    assert_eq!(name("www.Example.com.").to_string(), "www.Example.com");
    let odd = Name { wire: vec![3, b'a', b'.', b'b', 2, b'\\', 0x07, 1, b' ', 0] };
    assert_eq!(odd.to_string(), "a\\046b.\\092\\007.\\032");
    assert_eq!(Name { wire: vec![0] }.to_string(), ".", "the root");
}

// --- Real replies ---

#[test]
fn a_real_reply_with_two_addresses_gives_both_in_order() {
    assert_eq!(read(&REAL_GOOGLE, 0x1001, &name("dns.google"), MAX_ALIASES), answer(&[], &[[8, 8, 4, 4], [8, 8, 8, 8]]));
}

#[test]
fn rfc1034_3_6_2_a_real_chain_of_aliases_is_followed_to_its_address() {
    assert_eq!(
        read(&REAL_APPLE, 0x1000, &name("www.apple.com"), MAX_ALIASES),
        answer(
            &["www-apple-com.v.aaplimg.com", "www.apple.com.edgekey.net", "e6858.dsce9.akamaiedge.net"],
            &[[23, 32, 112, 246]]
        )
    );
    assert_eq!(
        read(&REAL_MICROSOFT, 0x1003, &name("www.microsoft.com"), MAX_ALIASES),
        answer(&["www.microsoft.com-c-3.edgekey.net", "e13678.dscb.akamaiedge.net"], &[[23, 212, 193, 218]])
    );
    assert_eq!(
        read(&REAL_APPLE, 0x1000, &name("www.apple.com"), 2),
        Verdict::TooManyAliases,
        "three aliases under a bound of two"
    );
}

#[test]
fn rfc6761_6_4_a_real_invalid_name_does_not_exist() {
    assert_eq!(read(&REAL_INVALID, 0x1002, &name("doesnotexist.invalid"), MAX_ALIASES), Verdict::NoSuchName);
}

// --- Whose reply it is (RFC 5452 §9.1) ---

#[test]
fn rfc5452_9_1_a_reply_is_read_only_for_its_id_and_its_question() {
    let asked = name("dns.google");
    assert_eq!(read(&REAL_GOOGLE, 0x1002, &asked, MAX_ALIASES), Verdict::Stray(Stray::Unasked));
    assert_eq!(read(&REAL_GOOGLE, 0x1001, &name("dns.googlf"), MAX_ALIASES), Verdict::Stray(Stray::OtherQuestion));
    assert_eq!(
        read(&REAL_GOOGLE, 0x1001, &name("DNS.Google"), MAX_ALIASES),
        answer(&[], &[[8, 8, 4, 4], [8, 8, 8, 8]]),
        "a server may echo the question in another case"
    );
    let mut other_type = REAL_GOOGLE;
    other_type[25] = 28;
    assert_eq!(read(&other_type, 0x1001, &asked, MAX_ALIASES), Verdict::Stray(Stray::OtherQuestion));
    let mut other_class = REAL_GOOGLE;
    other_class[27] = 3;
    assert_eq!(read(&other_class, 0x1001, &asked, MAX_ALIASES), Verdict::Stray(Stray::OtherQuestion));
    for qdcount in [0u8, 2] {
        let mut other = REAL_GOOGLE;
        other[5] = qdcount;
        assert_eq!(read(&other, 0x1001, &asked, MAX_ALIASES), Verdict::Stray(Stray::OtherQuestion), "QDCOUNT {qdcount}");
    }
}

#[test]
fn rfc1035_4_1_1_a_query_or_another_opcode_is_no_reply() {
    let asked = name("dns.google");
    let mut query = REAL_GOOGLE;
    query[2] &= 0x7f;
    assert_eq!(read(&query, 0x1001, &asked, MAX_ALIASES), Verdict::Stray(Stray::NotAResponse));
    for opcode in 1..16u8 {
        let mut other = REAL_GOOGLE;
        other[2] = (other[2] & 0x87) | (opcode << 3);
        assert_eq!(read(&other, 0x1001, &asked, MAX_ALIASES), Verdict::Stray(Stray::NotAResponse), "opcode {opcode}");
    }
}

#[test]
fn rfc2181_9_a_truncated_reply_is_refused_whatever_it_carries() {
    let mut cut = REAL_GOOGLE;
    cut[2] |= 0x02;
    assert_eq!(read(&cut, 0x1001, &name("dns.google"), MAX_ALIASES), Verdict::Truncated);
}

#[test]
fn rfc1035_4_1_1_every_rcode_is_named() {
    for rcode in 0..16u8 {
        let mut msg = REAL_GOOGLE;
        msg[3] = (msg[3] & 0xf0) | rcode;
        let expected = match rcode {
            0 => answer(&[], &[[8, 8, 4, 4], [8, 8, 8, 8]]),
            3 => Verdict::NoSuchName,
            _ => Verdict::ServerFailed(rcode),
        };
        assert_eq!(read(&msg, 0x1001, &name("dns.google"), MAX_ALIASES), expected, "rcode {rcode}");
    }
}

#[test]
fn rfc2308_2_2_a_name_with_no_address_is_an_empty_answer() {
    let msg = reply(7, OK, "mail.example", &[]);
    assert_eq!(read(&msg, 7, &name("mail.example"), MAX_ALIASES), answer(&[], &[]));
}

// --- Which records are read ---

#[test]
fn rfc2181_5_4_1_an_address_for_a_name_not_asked_is_not_read() {
    let msg = reply(
        7,
        OK,
        "www.example",
        &[a("bank.example", [6, 6, 6, 6]), a("www.example", [1, 2, 3, 4]), a("example", [6, 6, 6, 6])],
    );
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), answer(&[], &[[1, 2, 3, 4]]));
    // An address beside an alias belongs to the name that is not the chain's
    // end, and so is not read either (RFC 1034 §3.6.2: no other data at an
    // alias).
    let beside = reply(7, OK, "www.example", &[a("www.example", [6, 6, 6, 6]), cname("www.example", "cdn.example")]);
    assert_eq!(read(&beside, 7, &name("www.example"), MAX_ALIASES), answer(&["cdn.example"], &[]));
}

#[test]
fn rfc1034_3_6_2_a_chain_is_followed_in_any_order() {
    let msg = reply(
        7,
        OK,
        "www.example",
        &[
            a("c.example", [9, 9, 9, 9]),
            cname("b.example", "c.example"),
            a("c.example", [9, 9, 9, 9]),
            cname("www.example", "b.example"),
            a("c.example", [8, 8, 8, 8]),
        ],
    );
    assert_eq!(
        read(&msg, 7, &name("www.example"), MAX_ALIASES),
        answer(&["b.example", "c.example"], &[[9, 9, 9, 9], [8, 8, 8, 8]]),
        "each address once, in the reply's order"
    );
}

#[test]
fn a_loop_of_aliases_ends_at_the_bound() {
    let msg = reply(7, OK, "a.example", &[cname("a.example", "b.example"), cname("b.example", "a.example")]);
    assert_eq!(read(&msg, 7, &name("a.example"), MAX_ALIASES), Verdict::TooManyAliases);
    assert_eq!(read(&msg, 7, &name("a.example"), 0), Verdict::TooManyAliases);
}

#[test]
fn a_chain_exactly_at_the_bound_is_followed() {
    let hops: Vec<std::string::String> = (0..=MAX_ALIASES).map(|i| std::format!("h{i}.example")).collect();
    let mut answers: Vec<Vec<u8>> = hops.windows(2).map(|w| cname(&w[0], &w[1])).collect();
    answers.push(a(&hops[MAX_ALIASES], [5, 5, 5, 5]));
    let msg = reply(7, OK, &hops[0], &answers);
    let aliases: Vec<&str> = hops[1..].iter().map(|s| s.as_str()).collect();
    assert_eq!(read(&msg, 7, &name(&hops[0]), MAX_ALIASES), answer(&aliases, &[[5, 5, 5, 5]]));
    assert_eq!(read(&msg, 7, &name(&hops[0]), MAX_ALIASES - 1), Verdict::TooManyAliases);
}

#[test]
fn rfc2181_10_1_two_aliases_for_one_name_are_malformed() {
    let msg = reply(7, OK, "www.example", &[cname("www.example", "a.example"), cname("WWW.example", "b.example")]);
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), stray("two CNAME records for one name"));
}

#[test]
fn records_of_another_class_or_type_are_passed_over() {
    let msg = reply(
        7,
        OK,
        "www.example",
        &[rr(&wire("www.example"), TYPE_A, 3, &[6, 6, 6, 6]), rr(&wire("www.example"), 28, CLASS_IN, &[6; 16]), a("www.example", [1, 1, 1, 1])],
    );
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), answer(&[], &[[1, 1, 1, 1]]));
}

#[test]
fn rfc1035_3_4_1_an_address_record_is_four_bytes() {
    for len in [0usize, 3, 5, 16] {
        let msg = reply(7, OK, "www.example", &[rr(&wire("www.example"), TYPE_A, CLASS_IN, &vec![1; len])]);
        assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), stray("an A record whose data is not four bytes"), "{len}");
    }
}

#[test]
fn rfc1035_3_3_1_an_alias_record_is_one_name() {
    let mut data = wire("b.example");
    data.push(0);
    let msg = reply(7, OK, "www.example", &[rr(&wire("www.example"), TYPE_CNAME, CLASS_IN, &data)]);
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), stray("a CNAME record whose data is not one name"));
    // A name that runs past the record's own data.
    let mut spill = reply(7, OK, "www.example", &[rr(&wire("www.example"), TYPE_CNAME, CLASS_IN, &[3, b'a', b'b'])]);
    spill.extend_from_slice(&[b'c', 0]);
    assert_eq!(read(&spill, 7, &name("www.example"), MAX_ALIASES), stray("a CNAME record whose data is not one name"));
}

#[test]
fn rfc1035_4_1_3_a_record_or_a_count_past_the_end_is_malformed() {
    let mut long = reply(7, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    let len_at = long.len() - 6;
    long[len_at + 1] = 5;
    assert_eq!(read(&long, 7, &name("www.example"), MAX_ALIASES), stray("a read past the message's end"));
    let mut more = reply(7, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    more[7] = 2;
    assert_eq!(read(&more, 7, &name("www.example"), MAX_ALIASES), stray("a read past the message's end"));
}

// --- Compression (RFC 1035 §4.1.4) ---

/// A reply whose one answer's owner is `owner`, raw, placed after the
/// question for `www.example` at offset 12.
fn with_owner(owner: &[u8]) -> Vec<u8> {
    reply(7, OK, "www.example", &[rr(owner, TYPE_A, CLASS_IN, &[1, 2, 3, 4])])
}

const FIRST_ANSWER: u16 = 12 + 13 + 4;

#[test]
fn rfc1035_4_1_4_a_pointer_to_a_prior_name_is_read() {
    assert_eq!(read(&with_owner(&[0xc0, 12]), 7, &name("www.example"), MAX_ALIASES), answer(&[], &[[1, 2, 3, 4]]));
    // A label, then a pointer into the question's second label.
    let mut owner = vec![3, b'w', b'w', b'w'];
    owner.extend_from_slice(&[0xc0, 16]);
    assert_eq!(read(&with_owner(&owner), 7, &name("www.example"), MAX_ALIASES), answer(&[], &[[1, 2, 3, 4]]));
}

#[test]
fn rfc1035_4_1_4_a_pointer_that_does_not_point_back_is_refused() {
    let back = "a compression pointer that does not point back";
    let at = FIRST_ANSWER;
    for (what, owner) in [
        ("to itself", vec![0xc0 | (at >> 8) as u8, at as u8]),
        ("forward, past itself", vec![0xc0, (at + 20) as u8]),
        ("to the label before it", vec![1, b'a', 0xc0 | (at >> 8) as u8, at as u8]),
        ("past the message", vec![0xff, 0xff]),
    ] {
        assert_eq!(read(&with_owner(&owner), 7, &name("www.example"), MAX_ALIASES), stray(back), "{what}");
    }
    // Two names pointing at each other: whichever is read first, one of the
    // two pointers is forward of the name it ends.
    let mut msg = reply(7, OK, "www.example", &[]);
    msg[7] = 1;
    let first = msg.len();
    msg.extend_from_slice(&[1, b'x', 0xc0, (first + 4) as u8, 1, b'y', 0xc0, first as u8]);
    msg.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0, 0, 4, 1, 2, 3, 4]);
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), stray(back));
}

#[test]
fn rfc1035_4_1_4_the_two_reserved_label_types_are_refused() {
    for top in [0x40u8, 0x80] {
        assert_eq!(
            read(&with_owner(&[top | 1, b'a', 0]), 7, &name("www.example"), MAX_ALIASES),
            stray("a label type other than a length or a pointer"),
            "{top:#x}"
        );
    }
}

#[test]
fn rfc1035_2_3_4_a_name_built_past_255_bytes_by_pointers_is_refused() {
    // Five names in the data of a record of a type nobody reads, each one
    // 63-byte label ahead of the name before it: the fifth spells 321 bytes.
    let mut data = Vec::new();
    let mut starts = Vec::new();
    let data_at = FIRST_ANSWER as usize + 2 + 10;
    for i in 0..5 {
        starts.push(data_at + data.len());
        data.push(63);
        data.extend_from_slice(&[b'z'; 63]);
        match i {
            0 => data.push(0),
            _ => {
                let p = starts[i - 1];
                data.extend_from_slice(&[0xc0 | (p >> 8) as u8, p as u8]);
            }
        }
    }
    let pointing_at = |start: usize| {
        let mut msg = reply(7, OK, "www.example", &[rr(&[0xc0, 12], 99, CLASS_IN, &data)]);
        msg[7] = 2;
        let owner = [0xc0 | (start >> 8) as u8, start as u8];
        msg.extend_from_slice(&rr(&owner, TYPE_A, CLASS_IN, &[1, 2, 3, 4]));
        msg
    };
    assert_eq!(
        read(&pointing_at(starts[4]), 7, &name("www.example"), MAX_ALIASES),
        stray("a name longer than 255 bytes")
    );
    // Three of them are 193 bytes, and read.
    assert_eq!(
        read(&pointing_at(starts[2]), 7, &name("www.example"), MAX_ALIASES),
        answer(&[], &[]),
        "a 193-byte owner not asked about"
    );
}

/// RFC 1035 §2.3.4's bound read off the wire at its edge: an owner of exactly
/// 255 bytes is read, and one of 256 is refused, through the reader the
/// resolver and `toyos-mdns` share.
#[test]
fn rfc1035_2_3_4_an_owner_of_255_bytes_is_read_and_one_of_256_is_refused() {
    let owner = |last: usize| {
        let mut wire = Vec::new();
        for len in [63, 63, 63, last] {
            wire.push(len as u8);
            wire.extend(core::iter::repeat_n(b'z', len));
        }
        wire.push(0);
        wire
    };
    let (edge, past) = (owner(61), owner(62));
    assert_eq!((edge.len(), past.len()), (255, 256));
    let at = FIRST_ANSWER as usize;
    let msg = with_owner(&edge);
    let (read_back, after) = name_at(&msg, at).expect("a 255-byte owner");
    assert_eq!((read_back.wire(), after), (&edge[..], at + 255));
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), answer(&[], &[]), "255 bytes, not asked about");
    let msg = with_owner(&past);
    assert_eq!(name_at(&msg, at).map(|_| ()), Err(Stray::Malformed("a name longer than 255 bytes")));
    assert_eq!(read(&msg, 7, &name("www.example"), MAX_ALIASES), stray("a name longer than 255 bytes"));
}

// --- Every malformed input (the fuzz-like half) ---

/// A deterministic byte stream: xorshift64*, so a failing case is reproduced
/// by its seed.
struct Bytes(u64);

impl Bytes {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn byte(&mut self) -> u8 {
        (self.next() >> 56) as u8
    }
}

/// What every reading of any input owes: an answer's addresses are bytes the
/// message holds, and its chain is no longer than its bound. The reader
/// returning at all is the rest.
fn sound(msg: &[u8], verdict: &Verdict, budget: usize) {
    if let Verdict::Answer(answer) = verdict {
        assert!(answer.aliases.len() <= budget, "{} aliases under a bound of {budget}", answer.aliases.len());
        for addr in &answer.addrs {
            assert!(msg.windows(4).any(|w| w == addr), "{addr:?} is not in the message");
        }
    }
}

#[test]
fn every_prefix_of_a_real_reply_is_dropped_or_reads_as_the_whole() {
    for (msg, id, asked) in real() {
        let whole = read(msg, id, &asked, MAX_ALIASES);
        for len in 0..msg.len() {
            let verdict = read(&msg[..len], id, &asked, MAX_ALIASES);
            sound(&msg[..len], &verdict, MAX_ALIASES);
            assert!(
                matches!(verdict, Verdict::Stray(_)) || verdict == whole,
                "{len} of {} bytes read as {verdict:?}",
                msg.len()
            );
        }
    }
}

#[test]
fn every_byte_of_a_real_reply_set_to_every_value_is_read_soundly() {
    for (msg, id, asked) in real() {
        for at in 0..msg.len() {
            for value in 0..=255u8 {
                let mut changed = msg.to_vec();
                changed[at] = value;
                for budget in [0, 1, MAX_ALIASES] {
                    sound(&changed, &read(&changed, id, &asked, budget), budget);
                }
            }
        }
    }
}

#[test]
fn every_pair_of_bytes_in_the_header_and_question_is_read_soundly() {
    // The bytes every other decision stands on, in every pair of values at
    // every pair of positions: 256² per pair, one real reply's first 28.
    let asked = name("dns.google");
    let (sample, id) = (&REAL_GOOGLE, 0x1001);
    for i in (0..28).step_by(3) {
        for j in (i + 1..28).step_by(5) {
            for vi in 0..=255u8 {
                for vj in (0..=255u8).step_by(7) {
                    let mut changed = sample.to_vec();
                    changed[i] = vi;
                    changed[j] = vj;
                    sound(&changed, &read(&changed, id, &asked, MAX_ALIASES), MAX_ALIASES);
                }
            }
        }
    }
}

#[test]
fn random_answer_sections_behind_a_valid_question_are_read_soundly() {
    let asked = name("www.example");
    let mut rng = Bytes(0x9e37_79b9_7f4a_7c15);
    let head = reply(7, OK, "www.example", &[]);
    for _ in 0..200_000 {
        let mut msg = head.clone();
        let count = rng.byte() % 12;
        msg[7] = count;
        let len = (rng.next() % 300) as usize;
        for _ in 0..len {
            // Mostly small values and pointer bytes, which are where a name
            // reader goes wrong.
            let b = match rng.byte() % 4 {
                0 => 0xc0 | (rng.byte() & 0x01),
                1 => rng.byte() % 16,
                _ => rng.byte(),
            };
            msg.push(b);
        }
        let budget = (rng.byte() as usize) % (MAX_ALIASES + 1);
        sound(&msg, &read(&msg, 7, &asked, budget), budget);
    }
}

#[test]
fn random_bytes_are_never_an_answer_to_an_id_they_do_not_carry() {
    let asked = name("www.example");
    let mut rng = Bytes(0x243f_6a88_85a3_08d3);
    for _ in 0..200_000 {
        let len = (rng.next() % 600) as usize;
        let msg: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let id = rng.next() as u16;
        let verdict = read(&msg, id, &asked, MAX_ALIASES);
        sound(&msg, &verdict, MAX_ALIASES);
        if msg.len() < 2 || u16::from_be_bytes([msg[0], msg[1]]) != id {
            assert!(matches!(verdict, Verdict::Stray(_)), "{verdict:?}");
        }
    }
}

// --- The lookup ---

const S1: [u8; 4] = [10, 0, 2, 3];
const S2: [u8; 4] = [192, 168, 1, 1];

/// The query `step` sends: which one it is, its server and its ID.
fn asked(step: &Step) -> (Asked, [u8; 4], u16) {
    match step {
        Step::Ask { asked, to, query } => (*asked, *to, u16::from_be_bytes([query[0], query[1]])),
        other => panic!("expected a query, got {other:?}"),
    }
}

/// Where `step` sends its query, and with which ID.
fn to(step: &Step) -> ([u8; 4], u16) {
    let (_, to, id) = asked(step);
    (to, id)
}

fn waiting(lookup: &Lookup) -> Vec<Asked> {
    lookup.waiting().collect()
}

#[test]
fn rfc1035_4_2_1_every_server_is_asked_before_any_again_and_the_lookup_ends_after_its_rounds() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 100).unwrap();
    assert_eq!(to(&first), (S1, 100));
    assert_eq!(lookup.due(), WAIT_MS);
    assert_eq!(lookup.on_time(WAIT_MS - 1, || 999), Step::Wait, "before its wait is up");
    let mut now = WAIT_MS;
    let mut sent = vec![asked(&first)];
    loop {
        match lookup.on_time(now, || 101 + sent.len() as u16) {
            Step::Done(result) => {
                assert_eq!(result, Err(Failure::TimedOut));
                break;
            }
            step => sent.push(asked(&step)),
        }
        now += WAIT_MS;
    }
    let servers: Vec<[u8; 4]> = sent.iter().map(|s| s.1).collect();
    assert_eq!(servers, [S1, S2].repeat(ROUNDS), "round robin, {ROUNDS} rounds");
    let mut each: Vec<Asked> = sent.iter().map(|s| s.0).collect();
    each.sort();
    each.dedup();
    assert_eq!(each.len(), sent.len(), "every query is told apart from the others");
}

#[test]
fn rfc5452_9_1_only_the_asked_server_port_and_id_are_read() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 0x4242).unwrap();
    let (q, ..) = asked(&first);
    assert_eq!(to(&first), (S1, 0x4242));
    let good = reply(0x4242, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q, S2, PORT, &good, 5, || 1), Step::Wait, "another server");
    assert_eq!(lookup.on_datagram(q, S1, 5353, &good, 5, || 1), Step::Wait, "another port");
    assert_eq!(lookup.on_datagram(Asked(1), S1, PORT, &good, 5, || 1), Step::Wait, "another query's port");
    let forged = reply(0x4243, OK, "www.example", &[a("www.example", [6, 6, 6, 6])]);
    assert_eq!(lookup.on_datagram(q, S1, PORT, &forged, 5, || 1), Step::Wait, "another ID");
    assert_eq!(lookup.on_datagram(q, S1, PORT, &[0x42], 5, || 1), Step::Wait, "a byte");
    let elsewhere = reply(0x4242, OK, "bank.example", &[a("bank.example", [6, 6, 6, 6])]);
    assert_eq!(lookup.on_datagram(q, S1, PORT, &elsewhere, 5, || 1), Step::Wait, "another question");
    assert_eq!(lookup.on_datagram(q, S1, PORT, &good, 5, || 1), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

#[test]
fn a_late_answer_to_an_earlier_query_still_ends_the_lookup() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_time(WAIT_MS, || 2);
    assert_eq!(to(&second), (S2, 2));
    let (q2, ..) = asked(&second);
    assert_eq!(waiting(&lookup), [q1, q2], "the first query is still answered");
    let late = reply(1, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q1, S2, PORT, &late, WAIT_MS + 1, || 3), Step::Wait, "ID 1 went to the first server");
    assert_eq!(lookup.on_datagram(q2, S1, PORT, &late, WAIT_MS + 1, || 3), Step::Wait, "ID 1 left from the first query's port");
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &late, WAIT_MS + 1, || 3), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

#[test]
fn a_server_failure_asks_the_next_server_at_once() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let servfail = reply(1, OK | 2, "www.example", &[]);
    let second = lookup.on_datagram(q1, S1, PORT, &servfail, 10, || 2);
    assert_eq!(to(&second), (S2, 2));
    let (q2, ..) = asked(&second);
    assert_eq!(waiting(&lookup), [q2], "the failed query is answered no more");
    assert_eq!(lookup.due(), 10 + WAIT_MS);
    // The same failure again is no longer a query of this lookup's.
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &servfail, 11, || 3), Step::Wait);
    // An older query's failure while the newest still waits asks nobody.
    let third = lookup.on_time(10 + WAIT_MS, || 3);
    assert_eq!(to(&third), (S1, 3));
    let (q3, ..) = asked(&third);
    let old = reply(2, OK | 5, "www.example", &[]);
    assert_eq!(lookup.on_datagram(q2, S2, PORT, &old, 10 + WAIT_MS + 1, || 4), Step::Wait);
    assert_eq!(waiting(&lookup), [q3]);
    let ok = reply(3, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q3, S1, PORT, &ok, 10 + WAIT_MS + 2, || 4), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

#[test]
fn a_lookup_whose_every_server_failed_says_how() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 0).unwrap();
    let (mut q, _, mut id) = asked(&first);
    let mut now = 0;
    for _ in 0..ROUNDS - 1 {
        let step = lookup.on_datagram(q, S1, PORT, &reply(id, OK | 2, "www.example", &[]), now, || id + 1);
        assert_eq!(to(&step), (S1, id + 1));
        q = asked(&step).0;
        id += 1;
        now += 1;
    }
    assert_eq!(
        lookup.on_datagram(q, S1, PORT, &reply(id, OK | 2, "www.example", &[]), now, || id + 1),
        Step::Done(Err(Failure::ServerFailed(2)))
    );
}

/// The failure a lookup reports is the name it now asks: a server that could
/// not answer the name given says nothing about the alias it led to, so a
/// lookup restarted at that alias whose every query then goes unanswered ends
/// timed out.
#[test]
fn a_server_failure_before_an_alias_is_not_the_aliases_failure() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_datagram(asked(&first).0, S1, PORT, &reply(1, OK | 2, "www.example", &[]), 10, || 2);
    assert_eq!(to(&second), (S2, 2));
    let alias = reply(2, OK, "www.example", &[cname("www.example", "cdn.example")]);
    let restart = lookup.on_datagram(asked(&second).0, S2, PORT, &alias, 20, || 3);
    assert_eq!(to(&restart), (S1, 3), "the alias is asked afresh");
    let mut now = 20;
    let ended = loop {
        now += WAIT_MS;
        match lookup.on_time(now, || 4) {
            Step::Done(result) => break result,
            step => {
                asked(&step);
            }
        }
    };
    assert_eq!(ended, Err(Failure::TimedOut));
}

#[test]
fn a_truncated_reply_ends_the_lookup_by_name() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 9).unwrap();
    let cut = reply(9, OK | TC, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(asked(&first).0, S1, PORT, &cut, 1, || 10), Step::Done(Err(Failure::Truncated)));
}

#[test]
fn rfc2308_nxdomain_and_nodata_end_the_lookup_apart() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 9).unwrap();
    assert_eq!(
        lookup.on_datagram(asked(&first).0, S1, PORT, &reply(9, OK | 3, "www.example", &[]), 1, || 10),
        Step::Done(Err(Failure::NoSuchName))
    );
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 9).unwrap();
    assert_eq!(
        lookup.on_datagram(asked(&first).0, S1, PORT, &reply(9, OK, "www.example", &[]), 1, || 10),
        Step::Done(Err(Failure::NoAddress))
    );
}

#[test]
fn rfc1034_5_3_3_a_chain_that_ends_without_an_address_is_asked_again_at_its_end() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let alias = reply(1, OK, "www.example", &[cname("www.example", "cdn.example")]);
    let step = lookup.on_datagram(q1, S1, PORT, &alias, 50, || 2);
    let (q2, ..) = asked(&step);
    assert_eq!(step, Step::Ask { asked: q2, to: S1, query: query(2, &name("cdn.example")) }, "the first server, afresh");
    assert_eq!(waiting(&lookup), [q2], "no query for the old name is answered any more");
    assert_eq!(lookup.due(), 50 + WAIT_MS);
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &alias, 51, || 3), Step::Wait, "the old name's query is over");
    let done = reply(2, OK, "cdn.example", &[a("cdn.example", [4, 3, 2, 1])]);
    assert_eq!(lookup.on_datagram(q2, S1, PORT, &done, 52, || 3), Step::Done(Ok(vec![[4, 3, 2, 1]])));
}

#[test]
fn aliases_are_counted_across_the_names_a_lookup_asks() {
    let (mut lookup, first) = Lookup::start(name("h0.example"), &[S1], 0, || 0).unwrap();
    let (mut q, _, mut id) = asked(&first);
    for i in 0..MAX_ALIASES {
        let from = std::format!("h{i}.example");
        let to_name = std::format!("h{}.example", i + 1);
        let step = lookup.on_datagram(q, S1, PORT, &reply(id, OK, &from, &[cname(&from, &to_name)]), 0, || id + 1);
        assert_eq!(to(&step), (S1, id + 1), "alias {i}");
        q = asked(&step).0;
        id += 1;
    }
    let last = std::format!("h{MAX_ALIASES}.example");
    let one_more = reply(id, OK, &last, &[cname(&last, "end.example")]);
    assert_eq!(lookup.on_datagram(q, S1, PORT, &one_more, 0, || id + 1), Step::Done(Err(Failure::TooManyAliases)));
}

#[test]
fn a_lookup_with_no_server_does_not_start() {
    assert!(Lookup::start(name("www.example"), &[], 0, || 0).is_none());
}

/// A draw that must not be made.
fn undrawn() -> u16 {
    panic!("no query is sent, so no ID is drawn")
}

#[test]
fn rfc5452_9_2_an_id_is_drawn_for_a_query_that_is_sent_and_for_nothing_else() {
    let mut drawn = 0u16;
    let mut draw = || {
        drawn += 1;
        drawn
    };
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, &mut draw).unwrap();
    let (q, _, id) = asked(&first);
    assert_eq!(id, 1, "the first query carries the first draw");
    assert_eq!(lookup.on_time(WAIT_MS - 1, undrawn), Step::Wait, "a wait that is not over");
    assert_eq!(lookup.on_datagram(q, S2, PORT, &reply(1, OK, "www.example", &[]), 5, undrawn), Step::Wait, "a stray");
    for round in 1..ROUNDS {
        let step = lookup.on_time(round as u64 * WAIT_MS, &mut draw);
        assert_eq!(asked(&step).2, round as u16 + 1, "each later query carries the next draw");
    }
    assert_eq!(lookup.on_time(ROUNDS as u64 * WAIT_MS, undrawn), Step::Done(Err(Failure::TimedOut)), "the end sends nothing");
    assert_eq!(drawn, ROUNDS as u16, "one draw a query");

    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 7).unwrap();
    let answer = reply(7, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(asked(&first).0, S1, PORT, &answer, 1, undrawn), Step::Done(Ok(vec![[1, 2, 3, 4]])), "an answer");
    assert!(Lookup::start(name("www.example"), &[], 0, undrawn).is_none(), "a lookup that does not start");
}

// --- A query that did not reach its server ---
//
// RFC 1035 §4.2.1: "Queries sent using UDP may be lost, and hence a
// retransmission strategy is required", and of that strategy "The client
// should try other servers and server addresses before repeating a query to a
// specific address of a server" and "Too aggressive retransmission can easily
// slow responses for the community at large ... the minimum retransmission
// interval should be 2-5 seconds". A query the caller could not send was not
// lost: it never left.

#[test]
fn rfc1035_4_2_1_a_query_that_reached_nobody_is_not_waited_for_and_the_next_server_is_asked_at_once() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_unreached(q1, 7, || 2);
    assert_eq!(to(&second), (S2, 2), "the other server, with the next draw, before any wait");
    let (q2, ..) = asked(&second);
    assert_eq!(waiting(&lookup), [q2], "no answer is read for a query that reached nobody");
    assert_eq!(lookup.due(), 7 + WAIT_MS, "the wait is the second query's");
    let late = reply(1, OK, "www.example", &[a("www.example", [6, 6, 6, 6])]);
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &late, 8, undrawn), Step::Wait, "an answer to the query let go");
    assert_eq!(lookup.on_unreached(q1, 8, undrawn), Step::Wait, "the same report again is no query of this lookup's");
    let ok = reply(2, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q2, S2, PORT, &ok, 9, undrawn), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

#[test]
fn rfc1035_4_2_1_no_server_is_asked_again_at_once_a_lookup_that_reached_none_ends() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 1).unwrap();
    assert_eq!(lookup.on_unreached(asked(&first).0, 3_000, undrawn), Step::Done(Err(Failure::Unreachable)), "one server: nobody else to ask");

    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_unreached(asked(&first).0, 0, || 2);
    assert_eq!(to(&second), (S2, 2));
    assert_eq!(lookup.on_unreached(asked(&second).0, 0, undrawn), Step::Done(Err(Failure::Unreachable)), "two servers, each asked once and neither reached");
}

#[test]
fn rfc1035_4_2_1_a_server_not_reached_is_asked_again_in_its_turn_a_wait_later() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let mut sent = vec![(0, to(&lookup.on_unreached(asked(&first).0, 0, || 2)).0)];
    let mut now = 0;
    let ended = loop {
        now += WAIT_MS;
        // The wait of the query to S2 is over: S1's turn, and it is not reached.
        let step = match lookup.on_time(now, || 3) {
            Step::Done(result) => break result,
            step => step,
        };
        assert_eq!(to(&step).0, S1);
        match lookup.on_unreached(asked(&step).0, now, || 4) {
            Step::Done(result) => break result,
            step => sent.push((now, to(&step).0)),
        }
    };
    let rounds: Vec<(u64, [u8; 4])> = (0..ROUNDS as u64).map(|round| (round * WAIT_MS, S2)).collect();
    assert_eq!(sent, rounds, "S2 is asked once a round, a wait apart, S1 before it each time");
    assert_eq!((now, ended), (ROUNDS as u64 * WAIT_MS, Err(Failure::TimedOut)), "S2 was reached and waited for, so the lookup timed out");
}

#[test]
fn an_older_query_that_reached_nobody_asks_nobody_and_ends_nothing() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_time(WAIT_MS, || 2);
    let (q2, ..) = asked(&second);
    assert_eq!(lookup.on_unreached(q1, WAIT_MS + 1, undrawn), Step::Wait, "the newer query still waits for its own answer");
    assert_eq!(waiting(&lookup), [q2]);
    assert_eq!(lookup.due(), 2 * WAIT_MS, "and its wait is unchanged");
    assert_eq!(lookup.on_unreached(q2, WAIT_MS + 2, undrawn), Step::Done(Err(Failure::Unreachable)), "the newest reached nobody either");
}

// With a server left to ask at once, too: it is the newest query's wait.
#[test]
fn an_older_query_that_reached_nobody_does_not_hurry_the_next_server() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_time(WAIT_MS, || 2);
    assert_eq!(to(&second), (S2, 2));
    assert_eq!(lookup.on_unreached(asked(&first).0, WAIT_MS + 1, undrawn), Step::Wait, "the query to S2 still waits for its own answer");
    assert_eq!(waiting(&lookup), [asked(&second).0]);
    assert_eq!(lookup.due(), 2 * WAIT_MS);
}

// The newest query reached nobody, and nobody is left to ask at once: the
// lookup ends only where no older query's answer is still read, and otherwise
// waits the newest one's wait out for it.
#[test]
fn a_lookup_whose_newest_query_reached_nobody_waits_for_an_older_querys_answer() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_time(WAIT_MS, || 2);
    assert_eq!(lookup.on_unreached(asked(&second).0, WAIT_MS, undrawn), Step::Wait, "the first query may yet be answered");
    assert_eq!(waiting(&lookup), [q1]);
    assert_eq!(lookup.due(), 2 * WAIT_MS, "the wait is the one the query that reached nobody began");
    let ok = reply(1, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &ok, WAIT_MS + 1, undrawn), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

#[test]
fn a_lookup_that_then_reached_neither_server_waits_for_an_older_querys_answer() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_time(WAIT_MS, || 2);
    assert_eq!(to(&second), (S2, 2));
    let third = lookup.on_unreached(asked(&second).0, WAIT_MS, || 3);
    assert_eq!(to(&third), (S1, 3), "S1 again, a wait after its first query");
    assert_eq!(lookup.on_unreached(asked(&third).0, WAIT_MS, undrawn), Step::Wait, "the first query may yet be answered");
    assert_eq!(waiting(&lookup), [q1]);
    assert_eq!(lookup.due(), 2 * WAIT_MS);
    assert_eq!(lookup.on_time(2 * WAIT_MS - 1, undrawn), Step::Wait);
    assert_eq!(to(&lookup.on_time(2 * WAIT_MS, || 4)), (S2, 4), "S2's turn, a wait after it was last asked");
    let ok = reply(1, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &ok, 2 * WAIT_MS + 1, undrawn), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

// And the last query a lookup has: the queries before it are read until its
// wait is over, as if it had left.
#[test]
fn a_lookup_whose_last_query_reached_nobody_waits_for_the_answers_still_read() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let mut last = first;
    for round in 1..ROUNDS as u64 {
        last = lookup.on_time(round * WAIT_MS, || 2);
    }
    let end = ROUNDS as u64 * WAIT_MS;
    assert_eq!(lookup.on_unreached(asked(&last).0, end - WAIT_MS, undrawn), Step::Wait, "the queries before it may yet be answered");
    assert_eq!(waiting(&lookup).first(), Some(&q1));
    assert_eq!(lookup.on_time(end - 1, undrawn), Step::Wait);
    assert_eq!(lookup.on_time(end, undrawn), Step::Done(Err(Failure::TimedOut)), "queries that may have reached the server were waited for");
}

// --- A query the network says did not reach its server ---
//
// RFC 8085 §5.2: "applications SHOULD appropriately validate the payload of
// ICMP messages to ensure these are received in response to transmitted
// traffic ... This requires context, such as local state about communication
// instances to each destination, that although readily available in
// connection-oriented transport protocols is not always maintained by
// UDP-based applications", and "any incoming ICMP messages ... SHOULD NOT
// abort the communication". A report quotes eight octets of the query: its
// ports, and not its ID.

#[test]
fn rfc8085_5_2_a_reported_query_is_still_read_and_the_next_server_is_asked_at_once() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_report(q1, 7, || 2);
    assert_eq!(to(&second), (S2, 2), "the other server, before any wait");
    let (q2, ..) = asked(&second);
    assert_eq!(waiting(&lookup), [q1, q2], "the reported query's answer is still read");
    assert_eq!(lookup.due(), 7 + WAIT_MS);
    assert_eq!(lookup.on_report(q1, 8, undrawn), Step::Wait, "the same report again asks nobody");
    let ok = reply(1, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &ok, 9, undrawn), Step::Done(Ok(vec![[1, 2, 3, 4]])), "the reported query's answer");
}

// Every query is reported the moment it is sent, twice: each server is asked
// once a round and no sooner, and the lookup ends where one nobody answered
// ends.
#[test]
fn rfc8085_5_2_reports_alone_end_no_lookup_and_ask_no_server_twice_inside_a_wait() {
    for servers in [&[S1][..], &[S1, S2]] {
        let (mut lookup, first) = Lookup::start(name("www.example"), servers, 0, || 1).unwrap();
        let mut sent = vec![(0, to(&first).0)];
        let mut newest = asked(&first).0;
        let mut now = 0;
        let ended = loop {
            match lookup.on_report(newest, now, || 2) {
                Step::Ask { asked, to, .. } => {
                    sent.push((now, to));
                    newest = asked;
                    continue;
                }
                step => assert_eq!(step, Step::Wait, "a report ended the lookup"),
            }
            assert_eq!(lookup.on_report(newest, now, undrawn), Step::Wait, "the same report again");
            assert_eq!(lookup.on_time(now + WAIT_MS - 1, undrawn), Step::Wait, "the newest query's wait was cut short");
            now += WAIT_MS;
            match lookup.on_time(now, || 3) {
                Step::Done(result) => break result,
                step => {
                    sent.push((now, to(&step).0));
                    newest = asked(&step).0;
                }
            }
        };
        let rounds: Vec<(u64, [u8; 4])> = (0..ROUNDS as u64).flat_map(|round| servers.iter().map(move |server| (round * WAIT_MS, *server))).collect();
        assert_eq!(sent, rounds, "each server once a round, the rounds a wait apart");
        assert_eq!((now, ended), (ROUNDS as u64 * WAIT_MS, Err(Failure::TimedOut)));
        assert_eq!(waiting(&lookup).len(), ROUNDS * servers.len(), "every query's answer was read to the end");
    }
}

#[test]
fn rfc8085_5_2_a_report_of_an_older_query_asks_nobody() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_time(WAIT_MS, || 2);
    assert_eq!(lookup.on_report(asked(&first).0, WAIT_MS + 1, undrawn), Step::Wait, "the newer query still waits for its own answer");
    assert_eq!(lookup.due(), 2 * WAIT_MS, "and its wait is unchanged");
    assert_eq!(waiting(&lookup), [asked(&first).0, asked(&second).0]);
}

// What the caller knows ends what a report could not: a query reported and
// then known not to have left is let go like any other.
#[test]
fn a_reported_query_the_caller_then_knows_reached_nobody_is_let_go() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    assert_eq!(lookup.on_report(q1, 1, undrawn), Step::Wait, "one server: nobody else to ask");
    assert_eq!(lookup.on_unreached(q1, 2, undrawn), Step::Done(Err(Failure::Unreachable)));

    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_report(q1, 1, || 2);
    assert_eq!(lookup.on_unreached(asked(&second).0, 2, undrawn), Step::Wait, "the reported query may yet be answered");
    assert_eq!(waiting(&lookup), [q1]);
    assert_eq!(lookup.on_unreached(q1, 3, undrawn), Step::Done(Err(Failure::Unreachable)), "no query is left to read an answer for");
}

// --- Whoever reports or fails what ---
//
// RFC 1035 §4.2.1's interval is a server's: one that has not answered is not
// sent a second query inside it, whatever is said of its first and whatever
// another server answers. And a lookup does not end on another server's
// failure while a query's answer is read and its wait is not over.

// S1's every query is reported the moment it is sent, and S2 answers each of
// its own with a failure at once.
#[test]
fn reports_and_another_servers_failures_hurry_no_server_that_has_not_answered_and_end_no_lookup_early() {
    let refused = reply(2, OK | 5, "www.example", &[]);
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let mut sent = vec![(0, S1)];
    let mut newest = q1;
    let mut now = 0;
    let ended = loop {
        let step = lookup.on_report(newest, now, || 2);
        assert_eq!(to(&step), (S2, 2), "the other server, at once");
        sent.push((now, S2));
        assert_eq!(lookup.on_datagram(asked(&step).0, S2, PORT, &refused, now, || 9), Step::Wait, "S1 has not answered and was asked this millisecond");
        assert_eq!(lookup.on_report(newest, now, || 9), Step::Wait, "the same report again");
        assert_eq!(lookup.on_time(now + WAIT_MS - 1, || 9), Step::Wait, "the wait for S1 was cut short");
        now += WAIT_MS;
        match lookup.on_time(now, || 1) {
            Step::Done(result) => break result,
            step => {
                assert_eq!(to(&step), (S1, 1));
                sent.push((now, S1));
                newest = asked(&step).0;
            }
        }
    };
    let rounds: Vec<(u64, [u8; 4])> = (0..ROUNDS as u64).flat_map(|round| [(round * WAIT_MS, S1), (round * WAIT_MS, S2)]).collect();
    assert_eq!(sent, rounds, "each server once a round, the rounds a wait apart");
    assert_eq!((now, ended), (ROUNDS as u64 * WAIT_MS, Err(Failure::ServerFailed(5))));
    assert_eq!(waiting(&lookup).len(), ROUNDS, "every query to S1 was read to the end");

    // And S1's answer to its first query, a millisecond before that query's
    // wait is over, is the lookup's.
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_report(q1, 0, || 2);
    assert_eq!(lookup.on_datagram(asked(&second).0, S2, PORT, &refused, 0, || 9), Step::Wait);
    assert_eq!(waiting(&lookup), [q1]);
    let ok = reply(1, OK, "www.example", &[a("www.example", [1, 2, 3, 4])]);
    assert_eq!(lookup.on_datagram(q1, S1, PORT, &ok, WAIT_MS - 1, || 9), Step::Done(Ok(vec![[1, 2, 3, 4]])));
}

// The same of a query the caller knows did not leave: S1 is not asked again
// inside its interval because S2 failed, and with no answer read the lookup
// ends by S2's word.
#[test]
fn a_server_not_reached_is_not_asked_again_at_once_because_another_failed() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_unreached(asked(&first).0, 0, || 2);
    assert_eq!(to(&second), (S2, 2));
    let refused = reply(2, OK | 5, "www.example", &[]);
    assert_eq!(lookup.on_datagram(asked(&second).0, S2, PORT, &refused, 0, || 9), Step::Done(Err(Failure::ServerFailed(5))), "S1 was asked this millisecond, and no query is read");

    // With a query to S1 still read, the lookup waits for it instead.
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let (q1, ..) = asked(&first);
    let second = lookup.on_time(WAIT_MS, || 2);
    let third = lookup.on_datagram(asked(&second).0, S2, PORT, &refused, WAIT_MS, || 3);
    assert_eq!(to(&third), (S1, 3), "S1 again, a wait after its first query");
    let fourth = lookup.on_unreached(asked(&third).0, WAIT_MS, || 2);
    assert_eq!(to(&fourth), (S2, 2), "S2 answered, and is asked again at once like any server that fails");
    assert_eq!(lookup.on_datagram(asked(&fourth).0, S2, PORT, &refused, WAIT_MS, || 9), Step::Wait, "S1 was asked this millisecond");
    assert_eq!(waiting(&lookup), [q1]);
    assert_eq!(lookup.due(), 2 * WAIT_MS);
}

// What the caller knows of a server holds for an interval too: S1's query is
// known not to have left a second after S2 was asked, so when S2's wait ends
// the lookup waits on, its answer still read, until S1 may be asked.
#[test]
fn a_server_known_unreached_is_asked_again_a_wait_after_that_and_the_lookup_waits_for_its_turn() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_time(WAIT_MS, || 2);
    let (q2, ..) = asked(&second);
    assert_eq!(lookup.on_unreached(asked(&first).0, WAIT_MS + 1_000, || 9), Step::Wait);
    assert_eq!(lookup.on_time(2 * WAIT_MS, || 9), Step::Wait, "S1 was known unreached a second ago, and S2's answer is still read");
    assert_eq!(waiting(&lookup), [q2]);
    assert_eq!(lookup.due(), 2 * WAIT_MS + 1_000, "the wait is until S1 may be asked");
    assert_eq!(lookup.on_time(2 * WAIT_MS + 999, || 9), Step::Wait);
    assert_eq!(to(&lookup.on_time(2 * WAIT_MS + 1_000, || 3)), (S1, 3));
}

#[test]
fn a_server_that_answered_with_a_failure_is_what_a_lookup_that_then_reached_nobody_reports() {
    let (mut lookup, first) = Lookup::start(name("www.example"), &[S1, S2], 0, || 1).unwrap();
    let second = lookup.on_datagram(asked(&first).0, S1, PORT, &reply(1, OK | 2, "www.example", &[]), 1, || 2);
    assert_eq!(to(&second), (S2, 2));
    // S2 is not reached: S1 answered, so it is asked again at once, as after
    // any failure, and the count of queries nobody received starts at S2.
    let third = lookup.on_unreached(asked(&second).0, 2, || 3);
    assert_eq!(to(&third), (S1, 3));
    assert_eq!(lookup.on_unreached(asked(&third).0, 3, undrawn), Step::Done(Err(Failure::ServerFailed(2))));
}
