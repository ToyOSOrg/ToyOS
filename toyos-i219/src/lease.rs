//! What netd's lease probe answers, in the two places it can answer on a
//! machine whose console reaches nobody: its exit code, and a file of durable
//! lines on the log volume. One table both ends read.
//!
//! **The lease is the claim.** A DHCP lease is a DISCOVER this machine put on
//! the wire, an OFFER a server it does not control sent back, a REQUEST and an
//! ACK — frames out and frames in, answered by somebody else. The counts beside
//! it say how many of each the driver and the MAC's own statistics saw, so a
//! boot that got no lease still says which half of the exchange went missing.

use core::net::Ipv4Addr;

use crate::phy::Outcome;
use crate::{Counters, Link, Speed, Wire};

/// The code a probe that was leased an address exits with. Clear of
/// [`Outcome`]'s block, which ends at 82, and of 101, which a panicking netd
/// ends with.
pub const LEASED: i32 = 83;

const _: () = {
    let mut at = 0;
    while at < Outcome::ALL.len() {
        assert!(Outcome::ALL[at] as i32 != LEASED);
        at += 1;
    }
};

/// What one lease probe boot answers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// An address was leased inside the probe's window.
    Leased,
    /// None was, and this is what the bring-up and the link said: a PHY that
    /// came up with a link and still no lease is a network that did not
    /// answer, a PHY that never came up is the bring-up's refusal.
    NotLeased(Outcome),
}

impl Verdict {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Leased => LEASED,
            Self::NotLeased(outcome) => outcome.exit_code(),
        }
    }

    /// The verdict a code names, or `None` for one no probe exits with.
    pub fn from_exit_code(code: i32) -> Option<Self> {
        if code == LEASED {
            return Some(Self::Leased);
        }
        Outcome::from_exit_code(code).map(Self::NotLeased)
    }
}

impl core::fmt::Display for Verdict {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Leased => f.write_str("an address was leased: frames went out and came back"),
            Self::NotLeased(outcome) => write!(f, "no address was leased, and {outcome}"),
        }
    }
}

/// What the driver and the MAC counted, as one line spells them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    /// Transmit descriptors the part wrote back.
    pub sent: u32,
    /// Receive descriptors the part filled and the driver handed up.
    pub received: u32,
    /// The MAC's own statistics.
    pub wire: Wire,
}

impl Counts {
    pub fn of(counters: Counters, wire: Wire) -> Self {
        Self { sent: counters.sent, received: counters.received, wire }
    }
}

/// One thing the probe says, in the order it happened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event<'a> {
    /// The bring-up's own words about itself, one sentence per line.
    BroughtUp(&'a str),
    /// The link, whenever it changed.
    Link(Link),
    /// The lease, the moment it landed.
    Leased { address: Ipv4Addr, prefix: u8, server: Ipv4Addr, router: Option<Ipv4Addr> },
    Counts(Counts),
    /// The last line: the code the process ends with.
    Exit { code: i32 },
}

/// One line of the file: milliseconds since the process started, and what
/// happened then.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Line<'a> {
    pub ms: u64,
    pub event: Event<'a>,
}

impl core::fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ", self.ms)?;
        match self.event {
            Event::BroughtUp(words) => write!(f, "brought-up {words}"),
            Event::Link(Link::Down) => f.write_str("link down"),
            Event::Link(Link::Up { speed, full_duplex }) => write!(
                f,
                "link up {} {}",
                speed.mbps(),
                if full_duplex { "full" } else { "half" }
            ),
            Event::Leased { address, prefix, server, router } => {
                write!(f, "leased {address}/{prefix} from {server} router ")?;
                match router {
                    Some(router) => write!(f, "{router}"),
                    None => f.write_str("none"),
                }
            }
            Event::Counts(Counts { sent, received, wire }) => write!(
                f,
                "counts sent {sent} received {received} wire-sent {} wire-received {} \
                 wire-seen {} missed {} crc-errors {}",
                wire.sent, wire.received, wire.seen, wire.missed, wire.crc_errors
            ),
            Event::Exit { code } => write!(f, "exit {code}"),
        }
    }
}

impl<'a> Line<'a> {
    pub fn parse(text: &'a str) -> Option<Self> {
        let (ms, rest) = text.split_once(' ')?;
        let ms = ms.parse().ok()?;
        let (word, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let event = match word {
            "brought-up" if !rest.is_empty() => Event::BroughtUp(rest),
            "link" => Event::Link(parse_link(rest)?),
            "leased" => parse_lease(rest)?,
            "counts" => Event::Counts(parse_counts(rest)?),
            "exit" => Event::Exit { code: rest.parse().ok()? },
            _ => return None,
        };
        Some(Self { ms, event })
    }
}

fn parse_link(text: &str) -> Option<Link> {
    let mut words = text.split(' ');
    let link = match (words.next()?, words.next(), words.next()) {
        ("down", None, None) => Link::Down,
        ("up", Some(mbps), Some(duplex)) => Link::Up {
            speed: match mbps {
                "10" => Speed::Mbps10,
                "100" => Speed::Mbps100,
                "1000" => Speed::Mbps1000,
                _ => return None,
            },
            full_duplex: match duplex {
                "full" => true,
                "half" => false,
                _ => return None,
            },
        },
        _ => return None,
    };
    words.next().is_none().then_some(link)
}

fn parse_lease(text: &str) -> Option<Event<'_>> {
    let mut words = text.split(' ');
    let (address, prefix) = words.next()?.split_once('/')?;
    let (from, server, keyword, router) =
        (words.next()?, words.next()?, words.next()?, words.next()?);
    if from != "from" || keyword != "router" || words.next().is_some() {
        return None;
    }
    Some(Event::Leased {
        address: address.parse().ok()?,
        prefix: prefix.parse().ok()?,
        server: server.parse().ok()?,
        router: match router {
            "none" => None,
            router => Some(router.parse().ok()?),
        },
    })
}

fn parse_counts(text: &str) -> Option<Counts> {
    let mut words = text.split(' ');
    let mut field = |name: &str| -> Option<u64> {
        (words.next()? == name).then_some(())?;
        words.next()?.parse().ok()
    };
    let counts = Counts {
        sent: u32::try_from(field("sent")?).ok()?,
        received: u32::try_from(field("received")?).ok()?,
        wire: Wire {
            sent: field("wire-sent")?,
            received: field("wire-received")?,
            seen: field("wire-seen")?,
            missed: field("missed")?,
            crc_errors: field("crc-errors")?,
        },
    };
    words.next().is_none().then_some(counts)
}

/// What a whole file says: the lease it recorded, the last counts, and the
/// code it ended with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Summary {
    pub lease: Option<(u64, Event<'static>)>,
    pub counts: Option<Counts>,
    pub exit: Option<i32>,
}

/// Why a file is not a probe's report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unreadable<'a> {
    pub line: &'a str,
}

impl core::fmt::Display for Unreadable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} ends in a newline and is no line of a lease probe's report", self.line)
    }
}

/// Read a whole file. **A last line with no newline after it is the write the
/// machine ended in**, and is left out rather than refused.
pub fn summary(text: &str) -> Result<Summary, Unreadable<'_>> {
    let whole = text.rfind('\n').map_or("", |end| &text[..=end]);
    let mut summary = Summary { lease: None, counts: None, exit: None };
    for line in whole.lines() {
        let parsed = Line::parse(line).ok_or(Unreadable { line })?;
        match parsed.event {
            Event::Leased { address, prefix, server, router } if summary.lease.is_none() => {
                summary.lease =
                    Some((parsed.ms, Event::Leased { address, prefix, server, router }));
            }
            Event::Counts(counts) => summary.counts = Some(counts),
            Event::Exit { code } => summary.exit = Some(code),
            _ => {}
        }
    }
    Ok(summary)
}
