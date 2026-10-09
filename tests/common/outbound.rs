//! The T14 on its own cable: whether it reached its router, and the internet.
//! Both rows judge one boot by what its `outbound` job said in the log the
//! stick came back with, and by the kernel's record of handing the card over.
//!
//! **A verdict quotes the job's lines and nothing else of the log**, each
//! written again from what it read as ([`said::Line`]), which holds no
//! address: netstack's own lines name the network the machine is on, and they
//! stay on the stick.
//!
//! **The router row is red by the first step that failed**, in the order a
//! frame needs them: the card, its link, the lease, the router's link address,
//! the resolver on the link. The internet row is judged only over a green
//! router row, so one cause reds one row.

#[path = "../toyos-rust-tests/src/outbound_said.rs"]
#[allow(dead_code, reason = "the job calls `resolver`, and here only `toyos-checks` does")]
pub mod said;

use said::{Anchor, Connect, Driver, Frames, Line, Link, Lookup, Neighbour, Resolver, Word};
use toyos_build::bootlog;

use super::claims::I219;
use super::serial::Serial;

/// Whose lines the job's are: it writes into the ring of the runner that
/// spawned it.
const RUNNER: &str = "test-runner";

/// What the job said, each subject at most once.
struct Said {
    lines: Vec<Line>,
}

struct Card {
    driver: Driver,
    link: Link,
    sent: Frames,
    received: Frames,
}

struct Lease {
    held: bool,
    router: bool,
    resolver: Resolver,
}

#[derive(Clone, Copy)]
struct Reached {
    anchor: Anchor,
    lookup: Lookup,
    connect: Connect,
}

impl std::fmt::Display for Reached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: lookup {}, connect {}", self.anchor.word(), self.lookup.word(), self.connect.word())
    }
}

impl Said {
    fn of(log: &Serial) -> Result<Self, String> {
        let mut lines: Vec<Line> = Vec::new();
        let said = log
            .text()
            .lines()
            .filter_map(toyos_logstream::program_line)
            .filter(|said| said.tag == RUNNER)
            .filter_map(|said| Line::read(said.text));
        for (nth, line) in said.enumerate() {
            let line = line.ok_or_else(|| {
                format!(
                    "the job's `{}` line {} is none its vocabulary writes. It is not quoted: a line \
                     outside the vocabulary can hold anything",
                    said::HEAD.trim_end(),
                    nth + 1
                )
            })?;
            let subject = |line: &Line| match line {
                Line::Anchor { anchor, .. } => (std::mem::discriminant(line), Some(*anchor)),
                other => (std::mem::discriminant(other), None),
            };
            if let Some(first) = lines.iter().find(|had| subject(had) == subject(&line)) {
                return Err(format!("the job said one thing twice:\n    {first}\n    {line}"));
            }
            lines.push(line);
        }
        Ok(Self { lines })
    }

    /// Why a line the job owes is not there.
    fn ended_before(&self, what: &str) -> String {
        format!(
            "the job ended before it said {what}: its runner's bound ended it or it ended itself, and \
             its own lines in this boot's log say which"
        )
    }

    fn netstack(&self) -> Result<Word, String> {
        self.lines
            .iter()
            .find_map(|line| match line {
                Line::Netstack(word) => Some(*word),
                _ => None,
            })
            .ok_or_else(|| self.ended_before("netstack's word on its lease"))
    }

    fn card(&self) -> Result<Card, String> {
        self.lines
            .iter()
            .find_map(|line| match *line {
                Line::Card { driver, link, sent, received } => Some(Card { driver, link, sent, received }),
                _ => None,
            })
            .ok_or_else(|| self.ended_before("what card netstack drives"))
    }

    fn lease(&self) -> Result<Lease, String> {
        self.lines
            .iter()
            .find_map(|line| match *line {
                Line::Lease { held, router, resolver } => Some(Lease { held, router, resolver }),
                _ => None,
            })
            .ok_or_else(|| self.ended_before("what lease netstack holds"))
    }

    fn anchors(&self) -> Result<Vec<Reached>, String> {
        Anchor::ALL
            .iter()
            .map(|wanted| {
                self.lines
                    .iter()
                    .find_map(|line| match *line {
                        Line::Anchor { anchor, lookup, connect } if anchor == *wanted => {
                            Some(Reached { anchor, lookup, connect })
                        }
                        _ => None,
                    })
                    .ok_or_else(|| self.ended_before(&format!("what {} answered", wanted.word())))
            })
            .collect()
    }

    fn gateway(&self) -> Result<Neighbour, String> {
        self.lines
            .iter()
            .find_map(|line| match line {
                Line::Gateway(neighbour) => Some(*neighbour),
                _ => None,
            })
            .ok_or_else(|| self.ended_before("the router's neighbour entry"))
    }

    fn quoted(&self) -> String {
        self.lines.iter().map(|line| format!("\n    {line}")).collect()
    }
}

/// The machine reached its router.
pub fn router(kernel: &Serial, log: &Serial) -> Result<(), String> {
    let said = Said::of(log)?;
    reached_router(kernel, &said).map_err(|why| format!("{why}\n  the job said:{}", said.quoted()))?;
    eprintln!("  [outbound] the job said:{}", said.quoted());
    Ok(())
}

fn reached_router(kernel: &Serial, said: &Said) -> Result<(), String> {
    let handed = format!("[{I219}] handed over on slot ");
    let handed_over = kernel
        .text()
        .lines()
        .filter_map(bootlog::message)
        .any(|record| record.starts_with("pcidev: PCI ") && record.contains(&handed));
    if !handed_over {
        return Err(format!("no card: the kernel recorded handing {I219} to no program"));
    }
    if said.netstack()? == Word::NoCard {
        return Err(format!(
            "no card: the kernel handed {I219} over, and netstack says it was endowed no card it drives"
        ));
    }
    let card = said.card()?;
    if card.driver != Driver::I219 {
        return Err(format!("netstack drives `{}`, which is not this machine's wired card", card.driver.word()));
    }
    let frames = format!("the card counts {} frame(s) sent and {} received", card.sent, card.received);
    if card.link != Link::Up {
        return Err(format!("no link: the card's link is `{}`; {frames}", card.link.word()));
    }
    let lease = said.lease()?;
    if !lease.held {
        return Err(match (card.sent.0, card.received.0) {
            (Some(0), _) => format!("no lease, and {frames}: ToyOS asked for none on a link that is up"),
            (_, Some(0)) => format!(
                "no lease, and {frames}: nothing on this wire answered, which is the cable, the switch \
                 or the DHCP server"
            ),
            _ => format!(
                "no lease, and {frames}: the wire carries traffic, and either no DHCP server answered \
                 ToyOS or ToyOS did not take its answer. netstack counts no DHCP message, so this log \
                 cannot tell which"
            ),
        });
    }
    if !lease.router {
        return Err("the lease names no router".to_string());
    }
    let anchors = said.anchors()?;
    // Whether anything left by the router: a lookup of a resolver that is the
    // router or stands behind it, or a connect, whose peer is no neighbour.
    let through = matches!(lease.resolver, Resolver::Router | Resolver::OffLink)
        || anchors.iter().any(|a| a.connect != Connect::NotTried);
    let gateway = said.gateway()?;
    if through && gateway.answered() == Some(false) {
        return Err(format!(
            "the router did not answer for its link address: netstack's neighbour entry for it is `{}`",
            gateway.word()
        ));
    }
    if matches!(lease.resolver, Resolver::Router | Resolver::OnLink) {
        // Any answer but silence: a resolver that says no has answered.
        let answered =
            |a: &Reached| matches!(a.lookup, Lookup::Addresses | Lookup::NoAddress | Lookup::Failed);
        if !anchors.iter().any(answered) {
            let stands = match lease.resolver {
                Resolver::Router => "is the router",
                _ => "stands on the link",
            };
            let whose = if anchors.iter().all(|a| a.lookup == Lookup::Timeout) {
                "it answered neither lookup"
            } else {
                "netstack ended a lookup itself, which is ToyOS's own refusal"
            };
            return Err(format!("the resolver the lease names {stands}, and {whose}"));
        }
    }
    if !said.lines.contains(&Line::Done) {
        return Err(said.ended_before("its last line"));
    }
    Ok(())
}

/// The machine reached one anchor. Judged only where [`router`] is green: a
/// red there is this row's cause too, and is said once.
pub fn internet(kernel: &Serial, log: &Serial) -> Result<(), String> {
    let Ok(said) = Said::of(log) else {
        eprintln!("  [outbound] the internet is not judged: the router row is red");
        return Ok(());
    };
    if reached_router(kernel, &said).is_err() {
        eprintln!("  [outbound] the internet is not judged: the router row is red");
        return Ok(());
    }
    let anchors = said.anchors()?;
    let connected: Vec<&str> =
        anchors.iter().filter(|a| a.connect == Connect::Connected).map(|a| a.anchor.word()).collect();
    if !connected.is_empty() {
        eprintln!("  [outbound] connected to port 443 of {}", connected.join(" and "));
        return Ok(());
    }
    let each = anchors.iter().map(|a| format!("\n    {a}")).collect::<String>();
    let said_no = |a: &Reached| {
        matches!(a.connect, Connect::Refused | Connect::Reset) || matches!(a.lookup, Lookup::NoAddress | Lookup::Failed)
    };
    let own = |a: &Reached| {
        matches!(a.lookup, Lookup::NoResolver | Lookup::Refused) || matches!(a.connect, Connect::NoAddress | Connect::Error)
    };
    let whose = if anchors.iter().any(said_no) {
        "an answer came back and it was no, so what ToyOS sent was carried and answered: the uplink or \
         the service"
    } else if said.lease()?.resolver == Resolver::None {
        "the lease names no resolver, so no name could be looked up: the network's DHCP server"
    } else if anchors.iter().any(own) {
        "netstack ended a request itself, which is ToyOS's own refusal"
    } else {
        "both were silent, and silence from beyond the link is the same whether ToyOS's packets were \
         wrong or the uplink carried none: this log cannot tell ToyOS from the uplink"
    };
    Err(format!("neither anchor connected over a green router row; {whose}{each}"))
}
