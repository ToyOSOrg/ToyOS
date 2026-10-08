//! The outbound rows' judges over logs written by hand, one per verdict, and
//! the job's vocabulary: every line it can write reads back, and none holds an
//! address. Every address here is RFC 5737's and the MAC is locally
//! administered.

use super::*;

use outbound::said::{self, Anchor, Asked, Connect, Driver, Frames, Line, Link, Lookup, Neighbour, Resolver, Word};

/// The kernel's record of handing the card to netstack.
const HANDED: &str =
    "[2026-10-08 12:00:01  1.192 cpu0 kernel] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28\n";

/// netstack's own lines, which name the network and which no verdict quotes.
const NETSTACK: &str = "[2026-10-08 12:00:01  1.300 netstack] netstack: MAC 02:00:00:00:00:01
[2026-10-08 12:00:14 14.400 netstack] netstack: DHCP: lease 192.0.2.17/24 from 192.0.2.1, gateway 192.0.2.1, dns [192.0.2.1 198.51.100.53], 13100 ms after netstack came up
";

const LEASED: &[&str] = &[
    "netstack said=lease",
    "card driver=i219 link=up sent=41 received=57",
    "lease held=yes router=named resolver=router",
];

const GREEN: &[&str] = &[
    "netstack said=lease",
    "card driver=i219 link=up sent=41 received=57",
    "lease held=yes router=named resolver=router",
    "anchor name=dns.quad9.net lookup=addresses connect=connected",
    "anchor name=dns.google lookup=addresses connect=connected",
    "gateway neighbour=reachable",
    "done",
];

/// A boot's log: the kernel's records, netstack's lines, and the job's `said`
/// as the runner's ring carries them.
fn log(kernel: &str, said: &[&str]) -> (serial::Serial, serial::Serial) {
    let mut whole = format!("{kernel}{NETSTACK}");
    for (nth, line) in said.iter().enumerate() {
        // An anchor's line is its own thread's.
        let thread = if line.starts_with("anchor") { format!(" tid={nth}") } else { String::new() };
        whole.push_str(&format!("[2026-10-08 12:00:15 15.{nth:03} test-runner{thread} pid=9] outbound: {line}\n"));
    }
    (
        serial::Serial::named("the kernel's log", toyos_build::bootlog::kernel_records(&whole)),
        serial::Serial::named("the log", whole),
    )
}

/// `said` with the lines after the lease replaced by `rest`.
fn leased(rest: &[&'static str]) -> Vec<&'static str> {
    LEASED.iter().chain(rest).copied().collect()
}

/// `said` with the line opening with `head` replaced by `with`.
fn with(said: &[&'static str], head: &str, with: &'static str) -> Vec<&'static str> {
    assert!(said.iter().any(|line| line.starts_with(head)), "no line opens with {head:?}");
    said.iter().map(|line| if line.starts_with(head) { with } else { *line }).collect()
}

/// Whether `text` holds a dotted quad or a MAC.
fn holds_an_address(text: &str) -> bool {
    let numbers = |token: &str, by: char, count: usize, radix: u32| {
        let parts: Vec<&str> = token.split(by).collect();
        parts.len() == count && parts.iter().all(|p| !p.is_empty() && u8::from_str_radix(p, radix).is_ok())
    };
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':'))
        .any(|token| numbers(token.trim_matches('.'), '.', 4, 10) || numbers(token.trim_matches(':'), ':', 6, 16))
}

/// What a row must answer: green, or a red saying this.
type Want = Result<(), &'static str>;

fn judged(case: &str, row: &str, got: Result<(), String>, want: Want) {
    match (&got, want) {
        (Ok(()), Ok(())) => {}
        (Err(why), Err(saying)) if why.contains(saying) => {}
        _ => panic!("{case}: the {row} row answered {got:?}, and it has to answer {want:?}"),
    }
    if let Err(why) = got {
        assert!(!holds_an_address(&why), "{case}: the {row} row's verdict holds an address:\n{why}");
        assert!(!why.contains("toyos-t14"), "{case}: the {row} row's verdict quotes netstack:\n{why}");
    }
}

pub fn each_verdict_names_its_cause() {
    let no_lease = |card: &'static str| -> Vec<&'static str> {
        vec!["netstack said=no-lease", card, "lease held=no router=none resolver=none", "done"]
    };
    let silent = [
        "anchor name=dns.google lookup=addresses connect=timeout",
        "anchor name=dns.quad9.net lookup=addresses connect=timeout",
        "gateway neighbour=reachable",
        "done",
    ];
    let unanswered = [
        "anchor name=dns.google lookup=timeout connect=not-tried",
        "anchor name=dns.quad9.net lookup=timeout connect=not-tried",
    ];
    // The case, the kernel's records, what the job said, and each row's answer.
    let cases: Vec<(&str, &str, Vec<&'static str>, Want, Want)> = vec![
        ("green", HANDED, GREEN.to_vec(), Ok(()), Ok(())),
        (
            "green with the ring's line, which no row judges",
            HANDED,
            [&GREEN[..GREEN.len() - 1], &["ring full=3 wake_armed=3 wake_taken=2 unsent=not-asked taken=60", "done"]]
                .concat(),
            Ok(()),
            Ok(()),
        ),
        (
            "green on a stack that says nothing of its neighbours",
            HANDED,
            with(GREEN, "gateway", "gateway neighbour=not-asked"),
            Ok(()),
            Ok(()),
        ),
        (
            "no card",
            "",
            vec!["netstack said=no-card", "done"],
            Err("no card: the kernel recorded handing 8086:15fc to no program"),
            Ok(()),
        ),
        (
            "a card netstack does not find",
            HANDED,
            vec!["netstack said=no-card", "done"],
            Err("netstack says it was endowed no card it drives"),
            Ok(()),
        ),
        (
            "no link",
            HANDED,
            no_lease("card driver=i219 link=down sent=0 received=0"),
            Err("no link: the card's link is `down`; the card counts 0 frame(s) sent and 0 received"),
            Ok(()),
        ),
        (
            "nothing asked",
            HANDED,
            no_lease("card driver=i219 link=up sent=0 received=12"),
            Err("ToyOS asked for none on a link that is up"),
            Ok(()),
        ),
        (
            "no offer",
            HANDED,
            no_lease("card driver=i219 link=up sent=6 received=0"),
            Err("nothing on this wire answered"),
            Ok(()),
        ),
        (
            "an offer and no lease",
            HANDED,
            no_lease("card driver=i219 link=up sent=6 received=31"),
            Err("the card counts 6 frame(s) sent and 31 received: the wire carries traffic"),
            Ok(()),
        ),
        (
            "a lease with no router",
            HANDED,
            with(GREEN, "lease", "lease held=yes router=none resolver=on-link"),
            Err("the lease names no router"),
            Ok(()),
        ),
        (
            "the router's neighbour entry failed",
            HANDED,
            [
                &with(LEASED, "lease", "lease held=yes router=named resolver=off-link")[..],
                &unanswered,
                &["gateway neighbour=failed", "done"],
            ]
            .concat(),
            Err("the router did not answer for its link address: netstack's neighbour entry for it is `failed`"),
            Ok(()),
        ),
        (
            "a resolver at the router that is silent",
            HANDED,
            [&leased(&unanswered)[..], &["gateway neighbour=reachable", "done"]].concat(),
            Err("the resolver the lease names is the router, and it answered neither lookup"),
            Ok(()),
        ),
        (
            // Nothing left by the router, so its entry is not this red's cause.
            "a resolver on the link that is silent",
            HANDED,
            [
                &with(LEASED, "lease", "lease held=yes router=named resolver=on-link")[..],
                &unanswered,
                &["gateway neighbour=none", "done"],
            ]
            .concat(),
            Err("the resolver the lease names stands on the link, and it answered neither lookup"),
            Ok(()),
        ),
        (
            "a lookup netstack ended itself",
            HANDED,
            leased(&[
                "anchor name=dns.google lookup=refused connect=not-tried",
                "anchor name=dns.quad9.net lookup=timeout connect=not-tried",
                "gateway neighbour=reachable",
                "done",
            ]),
            Err("is the router, and netstack ended a lookup itself"),
            Ok(()),
        ),
        (
            "one anchor",
            HANDED,
            with(GREEN, "anchor name=dns.quad9.net", "anchor name=dns.quad9.net lookup=addresses connect=timeout"),
            Ok(()),
            Ok(()),
        ),
        ("both silent", HANDED, leased(&silent), Ok(()), Err("this log cannot tell ToyOS from the uplink")),
        (
            "silent behind a resolver beyond the link",
            HANDED,
            [
                &with(LEASED, "lease", "lease held=yes router=named resolver=off-link")[..],
                &unanswered,
                &["gateway neighbour=reachable", "done"],
            ]
            .concat(),
            Ok(()),
            Err("this log cannot tell ToyOS from the uplink"),
        ),
        (
            "both answered and refused",
            HANDED,
            leased(&[
                "anchor name=dns.google lookup=addresses connect=refused",
                "anchor name=dns.quad9.net lookup=addresses connect=reset",
                "gateway neighbour=reachable",
                "done",
            ]),
            Ok(()),
            Err("an answer came back and it was no"),
        ),
        (
            "a resolver that answers no",
            HANDED,
            leased(&[
                "anchor name=dns.google lookup=no-address connect=not-tried",
                "anchor name=dns.quad9.net lookup=failed connect=not-tried",
                "gateway neighbour=reachable",
                "done",
            ]),
            Ok(()),
            Err("the uplink or the service\n    dns.google: lookup no-address, connect not-tried"),
        ),
        (
            "a lease with no resolver",
            HANDED,
            [
                &with(LEASED, "lease", "lease held=yes router=named resolver=none")[..],
                &[
                    "anchor name=dns.google lookup=no-resolver connect=not-tried",
                    "anchor name=dns.quad9.net lookup=no-resolver connect=not-tried",
                    "gateway neighbour=none",
                    "done",
                ],
            ]
            .concat(),
            Ok(()),
            Err("the lease names no resolver"),
        ),
        (
            "a connect netstack ended itself",
            HANDED,
            leased(&[
                "anchor name=dns.google lookup=addresses connect=error",
                "anchor name=dns.quad9.net lookup=addresses connect=timeout",
                "gateway neighbour=reachable",
                "done",
            ]),
            Ok(()),
            Err("ToyOS's own refusal"),
        ),
        (
            "a job its runner ended",
            HANDED,
            GREEN[..GREEN.len() - 1].to_vec(),
            Err("the job ended before it said its last line"),
            Ok(()),
        ),
        (
            "a job ended inside an anchor",
            HANDED,
            GREEN[..4].to_vec(),
            Err("the job ended before it said what dns.google answered"),
            Ok(()),
        ),
        (
            "an address in a line",
            HANDED,
            with(GREEN, "lease", "lease held=yes router=192.0.2.1 resolver=router"),
            Err("line 3 is none its vocabulary writes"),
            Ok(()),
        ),
        (
            "a word after a line",
            HANDED,
            with(GREEN, "gateway", "gateway neighbour=reachable 02:00:00:00:00:01"),
            Err("line 6 is none its vocabulary writes"),
            Ok(()),
        ),
        (
            "one thing said twice",
            HANDED,
            [GREEN, &["anchor name=dns.google lookup=timeout connect=not-tried"]].concat(),
            Err("the job said one thing twice"),
            Ok(()),
        ),
    ];
    for (case, kernel, said, router, internet) in cases {
        let (kernel, log) = log(kernel, &said);
        judged(case, "router", outbound::router(&kernel, &log), router);
        judged(case, "internet", outbound::internet(&kernel, &log), internet);
    }

    // The log's own lines trip the scan the verdicts were held to.
    assert!(holds_an_address(NETSTACK) && holds_an_address("at 192.0.2.1.") && holds_an_address("02:00:00:00:00:01"));
    assert!(!holds_an_address("the card counts 6 frame(s) sent and 31 received: 8086:15fc on port 443"));

    // Lines another program says under the job's head are not the job's.
    let (kernel, theirs) = log(HANDED, GREEN);
    let theirs = serial::Serial::named("the log", theirs.text().replace(" test-runner", " netstack"));
    judged(
        "another program's lines",
        "router",
        outbound::router(&kernel, &theirs),
        Err("the job ended before it said netstack's word on its lease"),
    );
}

/// Every line the job can write reads back as itself and holds no address,
/// and text that is not such a line is not read as one.
pub fn no_line_holds_an_address() {
    let counts = [Frames(None), Frames(Some(0)), Frames(Some(19_216_811)), Frames(Some(u64::MAX))];
    let mut lines = vec![Line::Done];
    lines.extend(Word::ALL.iter().map(|word| Line::Netstack(*word)));
    lines.extend(Neighbour::ALL.iter().map(|neighbour| Line::Gateway(*neighbour)));
    for driver in Driver::ALL {
        for link in Link::ALL {
            for sent in counts {
                for received in counts {
                    lines.push(Line::Card { driver: *driver, link: *link, sent, received });
                }
            }
        }
    }
    for resolver in Resolver::ALL {
        for (held, router) in [(false, false), (false, true), (true, false), (true, true)] {
            lines.push(Line::Lease { held, router, resolver: *resolver });
        }
    }
    for anchor in Anchor::ALL {
        for lookup in Lookup::ALL {
            for connect in Connect::ALL {
                lines.push(Line::Anchor { anchor: *anchor, lookup: *lookup, connect: *connect });
            }
        }
    }
    for count in [Asked(None), Asked(Some(0)), Asked(Some(u64::MAX))] {
        lines.push(Line::Ring { full: count, wake_armed: Asked(Some(7)), wake_taken: count, unsent: Asked(None), taken: 60 });
    }
    for line in lines {
        let text = line.to_string();
        assert_eq!(Line::read(&text), Some(Some(line)), "{text}");
        assert!(!holds_an_address(&text), "{text}");
    }

    assert_eq!(Line::read("netstack: DHCP: lease 192.0.2.17/24"), None);
    for not_one in [
        "outbound: lease held=yes router=192.0.2.1 resolver=router",
        "outbound: card driver=i219 link=up sent=02:00:00:00:00:01 received=1",
        "outbound: card driver=i219 link=up sent=+1 received=1",
        "outbound: card driver=i219 link=up sent=01 received=1",
        "outbound: anchor name=192.0.2.9 lookup=addresses connect=connected",
        "outbound: anchor name=dns.google lookup=addresses connect=connected to 192.0.2.9",
        "outbound: gateway neighbour=reachable ",
        "outbound: ring full=1 wake_armed=1 wake_taken=1 unsent=0 taken=not-asked",
        "outbound: ring full=192.0.2.1 wake_armed=1 wake_taken=1 unsent=0 taken=60",
        "outbound: gateway 192.0.2.1",
        "outbound: done 192.0.2.1",
        "outbound: ",
    ] {
        assert_eq!(Line::read(not_one), Some(None), "{not_one}");
    }
}

/// The nearest resolver of a lease, off the words netstack writes for it.
pub fn a_resolver_stands_where_the_lease_puts_it() {
    let stands = |dns: &str| said::resolver("192.0.2.17/24", Some("192.0.2.1"), dns);
    assert_eq!(stands("192.0.2.1"), Some(Resolver::Router));
    assert_eq!(stands("192.0.2.53"), Some(Resolver::OnLink));
    assert_eq!(stands("198.51.100.53"), Some(Resolver::OffLink));
    assert_eq!(stands(""), Some(Resolver::None));
    // The nearest of several, whatever their order.
    assert_eq!(stands("198.51.100.53 192.0.2.53"), Some(Resolver::OnLink));
    assert_eq!(stands("192.0.2.53 198.51.100.53 192.0.2.1"), Some(Resolver::Router));
    // The prefix decides the link, at its ends too.
    assert_eq!(said::resolver("192.0.2.17/28", None, "192.0.2.53"), Some(Resolver::OffLink));
    assert_eq!(said::resolver("192.0.2.17/0", None, "203.0.113.9"), Some(Resolver::OnLink));
    assert_eq!(said::resolver("192.0.2.17/32", None, "192.0.2.17"), Some(Resolver::OnLink));
    // With no router named, the address a router would have is on the link.
    assert_eq!(said::resolver("192.0.2.17/24", None, "192.0.2.1"), Some(Resolver::OnLink));
    for (address, router, dns) in [
        ("192.0.2.17", None, "192.0.2.1"),
        ("192.0.2.17/33", None, "192.0.2.1"),
        ("192.0.2.17/24", Some("the-router"), "192.0.2.1"),
        ("192.0.2.17/24", None, "192.0.2.1,192.0.2.2"),
    ] {
        assert_eq!(said::resolver(address, router, dns), None, "{address} {router:?} {dns}");
    }
}
