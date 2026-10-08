//! What the `outbound` job says, and the whole of what a line of it can hold.
//!
//! The job writes [`Line`]s and the harness's judges (`tests/common/outbound.rs`)
//! read them back, each through this one file. **A line carries words of a
//! closed list and counts, and nothing else**: no address, no MAC and no name
//! of the network the machine is on can be put in one, so none reaches a
//! verdict that quotes it.
//!
//! Pure: `std` only.

use std::fmt;
use std::net::Ipv4Addr;

/// What opens every line.
pub const HEAD: &str = "outbound: ";

/// An enum whose every value is one word of a line.
macro_rules! words {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $value:ident = $word:literal,)+ }) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vdoc])* $value,)+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$value,)+];

            pub const fn word(self) -> &'static str {
                match self {
                    $(Self::$value => $word,)+
                }
            }

            pub fn read(word: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.word() == word)
            }
        }
    };
}

words! {
    /// A service the row asks for by name: a public resolver that takes
    /// anonymous connections on port 443.
    Anchor {
        Google = "dns.google",
        Quad9 = "dns.quad9.net",
    }
}

words! {
    /// netstack's own word about this machine's address, off the log.
    Word {
        Lease = "lease",
        NoLease = "no-lease",
        /// It was endowed no card and left.
        NoCard = "no-card",
    }
}

words! {
    Driver {
        I219 = "i219",
        E82574 = "82574",
        VirtioNet = "virtio-net",
        /// One this file has no word for.
        Other = "other",
    }
}

words! {
    Link {
        Up = "up",
        Down = "down",
        /// The driver is told nothing about its link.
        Unreported = "unreported",
    }
}

words! {
    /// Where the nearest resolver the lease names stands, nearest first.
    Resolver {
        /// It is the router.
        Router = "router",
        OnLink = "on-link",
        /// Every one is reached through the router.
        OffLink = "off-link",
        /// The lease names none.
        None = "none",
    }
}

words! {
    /// How one name's lookup ended.
    Lookup {
        Addresses = "addresses",
        /// A resolver answered that the name has no address.
        NoAddress = "no-address",
        /// netstack ended it with its word for anything else: on the stack the
        /// rows were first read on, a reply no address came of.
        Failed = "failed",
        /// No resolver answered.
        Timeout = "timeout",
        /// netstack holds no resolver to ask.
        NoResolver = "no-resolver",
        /// netstack refused the request for a reason of its own.
        Refused = "refused",
    }
}

words! {
    /// How one connect to port 443 ended.
    Connect {
        Connected = "connected",
        /// The peer, or something on the way to it, answered no.
        Refused = "refused",
        Reset = "reset",
        Timeout = "timeout",
        /// netstack holds no address to connect from.
        NoAddress = "no-address",
        /// netstack refused the request for a reason of its own.
        Error = "error",
        /// The lookup gave no address to connect to.
        NotTried = "not-tried",
    }
}

words! {
    /// The router's entry in netstack's neighbour table.
    Neighbour {
        Reachable = "reachable",
        Stale = "stale",
        Delay = "delay",
        Probe = "probe",
        Incomplete = "incomplete",
        Unreachable = "unreachable",
        Failed = "failed",
        /// The table holds no entry for it.
        None = "none",
        /// netstack's answer carries no word about it.
        NotAsked = "not-asked",
    }
}

impl Neighbour {
    /// Whether the router has answered for its link address.
    pub const fn answered(self) -> Option<bool> {
        match self {
            Self::Reachable | Self::Stale | Self::Delay | Self::Probe => Some(true),
            Self::Incomplete | Self::Unreachable | Self::Failed | Self::None => Some(false),
            Self::NotAsked => None,
        }
    }
}

/// A frame count off the card's own statistics, where the driver keeps them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frames(pub Option<u64>);

const UNREPORTED: &str = "unreported";

impl Frames {
    fn read(word: &str) -> Option<Self> {
        if word == UNREPORTED {
            return Some(Self(None));
        }
        // Digits and nothing else, which `parse` alone does not hold a word to.
        let digits = !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit());
        digits.then(|| word.parse().ok()).flatten().map(|count| Self(Some(count)))
    }
}

impl fmt::Display for Frames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(count) => write!(f, "{count}"),
            None => f.write_str(UNREPORTED),
        }
    }
}

/// A count netstack's answer carries only on a card that keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked(pub Option<u64>);

const NOT_ASKED: &str = "not-asked";

/// What follows a ring's `full=0`: the burst found room every time, so the
/// boot read nothing of a full ring or its wake.
const NEVER_FILLED: &str = " (the ring never filled: nothing read)";

impl Asked {
    fn read(word: &str) -> Option<Self> {
        if word == NOT_ASKED {
            return Some(Self(None));
        }
        Frames::read(word).filter(|count| count.0.is_some()).map(|count| Self(count.0))
    }
}

impl fmt::Display for Asked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(count) => write!(f, "{count}"),
            None => f.write_str(NOT_ASKED),
        }
    }
}

/// One line of the job's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    Netstack(Word),
    Card { driver: Driver, link: Link, sent: Frames, received: Frames },
    Lease { held: bool, router: bool, resolver: Resolver },
    Anchor { anchor: Anchor, lookup: Lookup, connect: Connect },
    Gateway(Neighbour),
    /// The card's transmit ring after a burst at the router: how often a frame
    /// found it full, how often its wake was armed and taken, the frames a
    /// link change gave back, the frames the ring and the wire count sent,
    /// the link's speed in Mb/s, and how many of the burst's datagrams
    /// netstack took.
    Ring {
        full: Asked,
        wake_armed: Asked,
        wake_taken: Asked,
        unsent: Asked,
        descriptors_sent: Asked,
        wire_sent: Asked,
        speed: Asked,
        taken: u64,
    },
    /// The job's last line.
    Done,
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(HEAD)?;
        match self {
            Self::Netstack(word) => write!(f, "netstack said={}", word.word()),
            Self::Card { driver, link, sent, received } => {
                write!(f, "card driver={} link={} sent={sent} received={received}", driver.word(), link.word())
            }
            Self::Lease { held, router, resolver } => write!(
                f,
                "lease held={} router={} resolver={}",
                if *held { "yes" } else { "no" },
                if *router { "named" } else { "none" },
                resolver.word()
            ),
            Self::Anchor { anchor, lookup, connect } => {
                write!(f, "anchor name={} lookup={} connect={}", anchor.word(), lookup.word(), connect.word())
            }
            Self::Gateway(neighbour) => write!(f, "gateway neighbour={}", neighbour.word()),
            Self::Ring { full, wake_armed, wake_taken, unsent, descriptors_sent, wire_sent, speed, taken } => {
                write!(f, "ring full={full}")?;
                if full.0 == Some(0) {
                    f.write_str(NEVER_FILLED)?;
                }
                write!(
                    f,
                    " wake_armed={wake_armed} wake_taken={wake_taken} unsent={unsent} \
                     descriptors_sent={descriptors_sent} wire_sent={wire_sent} speed={speed} taken={taken}"
                )
            }
            Self::Done => f.write_str("done"),
        }
    }
}

impl Line {
    /// `text` as a line of the job's: `None` for text [`HEAD`] does not open,
    /// and `Some(None)` for a line it opens that is not one a [`Line`] writes.
    pub fn read(text: &str) -> Option<Option<Self>> {
        let said = text.strip_prefix(HEAD)?;
        let line = Self::read_said(said);
        // Byte for byte what it reads as, so nothing rides a line beside its words.
        Some(line.filter(|line| line.to_string() == text))
    }

    fn read_said(said: &str) -> Option<Self> {
        // The one remark a line carries, which its writer puts back.
        let said = said.replacen(NEVER_FILLED, "", 1);
        let mut words = said.split(' ');
        let subject = words.next()?;
        let mut value = |key: &str| words.next()?.strip_prefix(key)?.strip_prefix('=');
        let yes = |word: &str, yes: &str, no: &str| match word {
            w if w == yes => Some(true),
            w if w == no => Some(false),
            _ => None,
        };
        Some(match subject {
            "netstack" => Self::Netstack(Word::read(value("said")?)?),
            "card" => Self::Card {
                driver: Driver::read(value("driver")?)?,
                link: Link::read(value("link")?)?,
                sent: Frames::read(value("sent")?)?,
                received: Frames::read(value("received")?)?,
            },
            "lease" => Self::Lease {
                held: yes(value("held")?, "yes", "no")?,
                router: yes(value("router")?, "named", "none")?,
                resolver: Resolver::read(value("resolver")?)?,
            },
            "anchor" => Self::Anchor {
                anchor: Anchor::read(value("name")?)?,
                lookup: Lookup::read(value("lookup")?)?,
                connect: Connect::read(value("connect")?)?,
            },
            "gateway" => Self::Gateway(Neighbour::read(value("neighbour")?)?),
            "ring" => Self::Ring {
                full: Asked::read(value("full")?)?,
                wake_armed: Asked::read(value("wake_armed")?)?,
                wake_taken: Asked::read(value("wake_taken")?)?,
                unsent: Asked::read(value("unsent")?)?,
                descriptors_sent: Asked::read(value("descriptors_sent")?)?,
                wire_sent: Asked::read(value("wire_sent")?)?,
                speed: Asked::read(value("speed")?)?,
                taken: Frames::read(value("taken")?)?.0?,
            },
            "done" => Self::Done,
            _ => return None,
        })
    }
}

/// Where the nearest of a lease's resolvers stands, off netstack's own words
/// for the lease: its address with its prefix length, its router where it
/// names one, and its resolvers with a space between two. `None` where a word
/// is not what netstack writes there.
pub fn resolver(address: &str, router: Option<&str>, dns: &str) -> Option<Resolver> {
    let (address, prefix) = address.split_once('/')?;
    let address = u32::from(address.parse::<Ipv4Addr>().ok()?);
    let prefix: u32 = prefix.parse().ok().filter(|len| *len <= 32)?;
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    let router = match router {
        Some(router) => Some(router.parse::<Ipv4Addr>().ok()?),
        None => None,
    };
    let mut nearest = Resolver::None;
    for server in dns.split_whitespace() {
        let server: Ipv4Addr = server.parse().ok()?;
        let stands = if Some(server) == router {
            Resolver::Router
        } else if u32::from(server) & mask == address & mask {
            Resolver::OnLink
        } else {
            Resolver::OffLink
        };
        let rank = |r: Resolver| Resolver::ALL.iter().position(|v| *v == r);
        if rank(stands) < rank(nearest) {
            nearest = stands;
        }
    }
    Some(nearest)
}
