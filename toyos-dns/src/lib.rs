//! A stub resolver's decisions (RFC 1035): the question it asks for a name's
//! IPv4 addresses, which datagram answers that question, what the answer says,
//! and when the question is asked again. Pure: `core` and `alloc`, no
//! `unsafe`, no I/O, and no clock or randomness of its own. The caller hands in
//! the time, every query ID and every datagram with its source.
//!
//! **A reply has crossed a trust boundary.** Anyone on the path can send a
//! datagram to the port a query left from, so a reply is read only if it
//! carries the ID of a query this lookup sent, comes from port 53 of the server
//! that query went to, and repeats the question (RFC 5452 §9.1). Anything else
//! is dropped and the lookup waits on. Every read is bounds-checked. A
//! compression pointer must point before the name it continues, because RFC
//! 1035 §4.1.4 points only to a *prior* occurrence, so a pointer loop is
//! refused rather than followed. Only answer records owned by the asked name,
//! or by an alias it leads to, are read (RFC 2181 §5.4.1), so a reply cannot
//! plant an address for a name it was not asked about.
//!
//! **IPv4 only.** `A` is the one type asked for, because the stack under netd
//! is built with IPv4 alone and an `AAAA` answer would name an address nothing
//! on this machine can reach.
//!
//! **A truncated reply is refused, not retried over TCP.** A reply with TC set
//! has left records out (RFC 2181 §9), so it is never read in part and the
//! lookup ends with [`Failure::Truncated`]. TCP (RFC 7766) is a second
//! transport with connection state of its own. It exists for answers past the
//! 512 bytes a UDP reply may carry (RFC 1035 §4.2.1), which an `A` question
//! for an ordinary name does not reach.
//!
//! **No cache.** Every lookup asks.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;

/// The port a server answers on (RFC 1035 §4.2.1).
pub const PORT: u16 = 53;

/// How long one query waits for its answer before the next is sent: the floor
/// of the two to five seconds RFC 1035 §4.2.1 recommends.
pub const WAIT_MS: u64 = 2_000;

/// How many times each server is asked for one name. Every server is asked
/// once before any is asked again (RFC 1035 §4.2.1).
pub const ROUNDS: usize = 3;

/// Aliases one lookup follows before it gives up. Policy: a chain is followed
/// to the name that holds the address (RFC 1034 §3.6.2), and a loop of aliases
/// is ended by this bound.
pub const MAX_ALIASES: usize = 8;

/// RFC 1035 §4.1.1.
const HEADER: usize = 12;
const QR: u16 = 0x8000;
const OPCODE: u16 = 0x7800;
const TC: u16 = 0x0200;
const RD: u16 = 0x0100;
const RCODE: u16 = 0x000f;
const NXDOMAIN: u8 = 3;

/// RFC 1035 §3.2.2 and §3.2.4.
const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const CLASS_IN: u16 = 1;

/// RFC 1035 §2.3.4: a label is at most 63 octets, and a name at most 255 in
/// its wire form, length octets and the root's zero included.
const MAX_LABEL: usize = 63;
const MAX_NAME: usize = 255;

/// A domain name in its uncompressed wire form: each label behind its length
/// octet, ending in the root's zero.
///
/// Compared without regard to ASCII case (RFC 1035 §2.3.3, RFC 4343). A length
/// octet is at most 63, below every ASCII letter, so folding the whole wire
/// form never changes one.
#[derive(Clone, Debug)]
pub struct Name {
    wire: Vec<u8>,
}

/// Why a name is not one this resolver asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadName {
    /// Nothing, or only the root: no host is named by it.
    Empty,
    /// Two dots together, or a leading dot.
    EmptyLabel,
    /// A label longer than 63 bytes.
    LongLabel,
    /// A name longer than 255 bytes in wire form.
    TooLong,
    /// A byte outside letters, digits, `-` and `_`. Host names are letters,
    /// digits and hyphens (RFC 1123 §2.1), and `_` is allowed because real
    /// names carry it. Anything else, internationalised names included, is
    /// refused by name rather than sent as raw bytes.
    Character(u8),
}

impl Name {
    /// `text` in the usual dotted form, with or without the root's trailing
    /// dot. No search list: the name asked is the name given.
    pub fn parse(text: &str) -> Result<Self, BadName> {
        let text = text.strip_suffix('.').unwrap_or(text);
        if text.is_empty() {
            return Err(BadName::Empty);
        }
        let mut wire = Vec::with_capacity(text.len() + 2);
        for label in text.split('.') {
            if label.is_empty() {
                return Err(BadName::EmptyLabel);
            }
            if label.len() > MAX_LABEL {
                return Err(BadName::LongLabel);
            }
            if let Some(&b) = label.as_bytes().iter().find(|&&b| !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
                return Err(BadName::Character(b));
            }
            wire.push(label.len() as u8);
            wire.extend_from_slice(label.as_bytes());
        }
        wire.push(0);
        if wire.len() > MAX_NAME {
            return Err(BadName::TooLong);
        }
        Ok(Self { wire })
    }

    /// The uncompressed wire form.
    pub fn wire(&self) -> &[u8] {
        &self.wire
    }

    /// The same name, whatever the case of its letters.
    pub fn same(&self, other: &Name) -> bool {
        self.wire.eq_ignore_ascii_case(&other.wire)
    }
}

/// A standard query (RFC 1035 §4.1.1, §4.1.2) with ID `id`, asking with
/// recursion desired for `name`'s `A` records.
pub fn query(id: u16, name: &Name) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + name.wire.len() + 4);
    for word in [id, RD, 1, 0, 0, 0] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out.extend_from_slice(&name.wire);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    out
}

/// Why a datagram is not the answer to a query, and so is dropped while the
/// lookup waits on (RFC 5452 §9.1; RFC 1035 §7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stray {
    /// Not from port 53 of a server a query of this lookup went to, or not
    /// carrying the ID that query did.
    Unasked,
    /// A query, not a response, or an opcode other than a standard query.
    NotAResponse,
    /// A response to some other question.
    OtherQuestion,
    /// A message this reader cannot take apart: a read past its end, a
    /// compression pointer that does not point back, a name too long, a
    /// record whose data is not what its type says.
    Malformed(&'static str),
}

/// What a response to the question says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Dropped: see [`Stray`].
    Stray(Stray),
    /// TC is set: records were left out, and none is read.
    Truncated,
    /// The server could not answer (RCODE 1, 2, 4, 5 or one RFC 1035 does
    /// not define), which asks the next server.
    ServerFailed(u8),
    /// RCODE 3: the name, or the name an alias leads to, does not exist
    /// (RFC 2308 §2.1).
    NoSuchName,
    /// The answer, read along the alias chain from the asked name.
    Answer(Answer),
    /// The alias chain is longer than the bound it was read under.
    TooManyAliases,
}

/// The records of an answer that are about the asked name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Aliases followed from the asked name, in order. The last is the name
    /// the addresses belong to.
    pub aliases: Vec<Name>,
    /// Addresses of the chain's last name, each once, in the reply's order.
    pub addrs: Vec<[u8; 4]>,
}

impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}

/// The dotted form, each byte that is not a printable character or is `.` or
/// `\` written as `\DDD` (RFC 1035 §5.1): a name read off the wire may hold
/// any byte.
impl core::fmt::Display for Name {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut at = 0;
        while self.wire[at] != 0 {
            let len = usize::from(self.wire[at]);
            if at != 0 {
                f.write_str(".")?;
            }
            for &b in &self.wire[at + 1..at + 1 + len] {
                match b {
                    0x21..=0x7e if b != b'.' && b != b'\\' => write!(f, "{}", b as char)?,
                    _ => write!(f, "\\{:03}", b)?,
                }
            }
            at += 1 + len;
        }
        if at == 0 {
            f.write_str(".")?;
        }
        Ok(())
    }
}

impl Eq for Name {}

/// Bounds-checked reads of one message.
struct Message<'a> {
    bytes: &'a [u8],
}

impl<'a> Message<'a> {
    fn slice(&self, at: usize, len: usize) -> Result<&'a [u8], Stray> {
        at.checked_add(len)
            .and_then(|end| self.bytes.get(at..end))
            .ok_or(Stray::Malformed("a read past the message's end"))
    }

    fn u16(&self, at: usize) -> Result<u16, Stray> {
        let b = self.slice(at, 2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// The name at `at`, and where the bytes after it begin.
    ///
    /// **A pointer must point before the run of labels it ends**, which is
    /// what a prior occurrence is (RFC 1035 §4.1.4). Each pointer then lowers
    /// the start of the run being read, so the walk ends after at most `at`
    /// pointers whatever the message says.
    fn name(&self, at: usize) -> Result<(Name, usize), Stray> {
        let mut wire = Vec::new();
        let mut pos = at;
        let mut run = at;
        let mut after = None;
        loop {
            let len = self.slice(pos, 1)?[0];
            match len & 0xc0 {
                0x00 if len == 0 => {
                    wire.push(0);
                    return Ok((Name { wire }, after.unwrap_or(pos + 1)));
                }
                0x00 => {
                    let label = self.slice(pos + 1, len as usize)?;
                    wire.push(len);
                    wire.extend_from_slice(label);
                    // Room is left for the root's zero.
                    if wire.len() >= MAX_NAME {
                        return Err(Stray::Malformed("a name longer than 255 bytes"));
                    }
                    pos += 1 + len as usize;
                }
                0xc0 => {
                    let low = self.slice(pos + 1, 1)?[0];
                    let target = usize::from(u16::from_be_bytes([len & 0x3f, low]));
                    if target >= run {
                        return Err(Stray::Malformed("a compression pointer that does not point back"));
                    }
                    after.get_or_insert(pos + 2);
                    run = target;
                    pos = target;
                }
                // 01 and 10 are reserved (RFC 1035 §4.1.4) or extended label
                // types this reader does not know (RFC 6891 §5).
                _ => return Err(Stray::Malformed("a label type other than a length or a pointer")),
            }
        }
    }
}

/// One resource record's fixed fields (RFC 1035 §4.1.3), and where its data
/// lies.
struct Record {
    owner: Name,
    rtype: u16,
    class: u16,
    data: usize,
    end: usize,
}

fn record(msg: &Message, at: usize) -> Result<Record, Stray> {
    let (owner, fixed) = msg.name(at)?;
    let rtype = msg.u16(fixed)?;
    let class = msg.u16(fixed + 2)?;
    let len = usize::from(msg.u16(fixed + 8)?);
    let data = fixed + 10;
    msg.slice(data, len)?;
    Ok(Record { owner, rtype, class, data, end: data + len })
}

/// What `msg`, a datagram that carries `id` from a server a query went to,
/// says to the question for `asked`. `aliases` is how many more aliases may be
/// followed before the chain is too long.
pub fn read(msg: &[u8], id: u16, asked: &Name, aliases: usize) -> Verdict {
    match read_checked(&Message { bytes: msg }, id, asked, aliases) {
        Ok(verdict) => verdict,
        Err(stray) => Verdict::Stray(stray),
    }
}

fn read_checked(msg: &Message, id: u16, asked: &Name, budget: usize) -> Result<Verdict, Stray> {
    if msg.u16(0)? != id {
        return Err(Stray::Unasked);
    }
    let flags = msg.u16(2)?;
    if flags & QR == 0 || flags & OPCODE != 0 {
        return Err(Stray::NotAResponse);
    }
    // The question first, so that a reply to another question is dropped
    // whatever else it says.
    if msg.u16(4)? != 1 {
        return Err(Stray::OtherQuestion);
    }
    let (question, after) = msg.name(HEADER)?;
    if !question.same(asked) || msg.u16(after)? != TYPE_A || msg.u16(after + 2)? != CLASS_IN {
        return Err(Stray::OtherQuestion);
    }
    if flags & TC != 0 {
        return Ok(Verdict::Truncated);
    }
    match (flags & RCODE) as u8 {
        0 => {}
        NXDOMAIN => return Ok(Verdict::NoSuchName),
        rcode => return Ok(Verdict::ServerFailed(rcode)),
    }

    let mut cnames: Vec<(Name, Name)> = Vec::new();
    let mut addrs: Vec<(Name, [u8; 4])> = Vec::new();
    let mut at = after + 4;
    for _ in 0..msg.u16(6)? {
        let r = record(msg, at)?;
        at = r.end;
        if r.class != CLASS_IN {
            continue;
        }
        match r.rtype {
            TYPE_A => {
                let data = msg.slice(r.data, r.end - r.data)?;
                let addr: [u8; 4] =
                    data.try_into().map_err(|_| Stray::Malformed("an A record whose data is not four bytes"))?;
                addrs.push((r.owner, addr));
            }
            TYPE_CNAME => {
                let (target, end) = msg.name(r.data)?;
                if end != r.end {
                    return Err(Stray::Malformed("a CNAME record whose data is not one name"));
                }
                // One alias per name (RFC 1034 §3.6.2, RFC 2181 §10.1): a
                // second leaves the chain with two ways to go.
                if cnames.iter().any(|(owner, _)| owner.same(&r.owner)) {
                    return Err(Stray::Malformed("two CNAME records for one name"));
                }
                cnames.push((r.owner, target));
            }
            _ => {}
        }
    }

    let mut chain: Vec<Name> = Vec::new();
    loop {
        let now = chain.last().unwrap_or(asked);
        let Some((_, target)) = cnames.iter().find(|(owner, _)| owner.same(now)) else { break };
        if chain.len() == budget {
            return Ok(Verdict::TooManyAliases);
        }
        chain.push(target.clone());
    }
    let owner = chain.last().unwrap_or(asked);
    let mut found: Vec<[u8; 4]> = Vec::new();
    for (_, addr) in addrs.iter().filter(|(name, _)| name.same(owner)) {
        if !found.contains(addr) {
            found.push(*addr);
        }
    }
    Ok(Verdict::Answer(Answer { aliases: chain, addrs: found }))
}

/// How a lookup ended without an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The name does not exist (RCODE 3).
    NoSuchName,
    /// The name exists and has no `A` record (RFC 2308 §2.2).
    NoAddress,
    /// Every query went unanswered for [`WAIT_MS`].
    TimedOut,
    /// The answer did not fit a UDP reply (TC).
    Truncated,
    /// Every server that answered could not answer, the last with this RCODE.
    ServerFailed(u8),
    /// The alias chain is longer than [`MAX_ALIASES`].
    TooManyAliases,
}

/// What the caller does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send `query` to port [`PORT`] of `to`, then wait until [`Lookup::due`].
    Ask { to: [u8; 4], query: Vec<u8> },
    /// Nothing to send: wait until [`Lookup::due`] or the next datagram.
    Wait,
    /// The lookup is over: the name's addresses, at least one, or why none.
    Done(Result<Vec<[u8; 4]>, Failure>),
}

/// One query sent for the name now asked.
#[derive(Clone, Copy, Debug)]
struct Sent {
    id: u16,
    to: [u8; 4],
}

/// One name's lookup, from its first query to its answer.
///
/// **Each query carries an ID the caller drew for it**, which the caller takes
/// from a source an off-path sender cannot predict (RFC 5452 §9.2). A reply to
/// any query this lookup sent for the name now asked is read, so an answer
/// late past its query's wait still ends the lookup.
#[derive(Debug)]
pub struct Lookup {
    /// The name now asked: the one given, or the last alias followed.
    name: Name,
    /// Aliases followed so far, counted against [`MAX_ALIASES`].
    aliases: usize,
    servers: Vec<[u8; 4]>,
    /// Queries sent for `name`, oldest first; one answered with a server
    /// failure is taken out.
    sent: Vec<Sent>,
    /// Queries sent for `name`, answered or not.
    asked: usize,
    /// When the newest query is given up on.
    due: u64,
    /// The RCODE of the last server failure, which is what a lookup that runs
    /// out of queries reports if any server answered at all.
    failed: Option<u8>,
}

impl Lookup {
    /// A lookup of `name` through `servers`, and its first query, sent at
    /// `now` with ID `id`. `None` when there is no server to ask.
    pub fn start(name: Name, servers: &[[u8; 4]], now: u64, id: u16) -> Option<(Self, Step)> {
        if servers.is_empty() {
            return None;
        }
        let mut lookup =
            Self { name, aliases: 0, servers: servers.to_vec(), sent: Vec::new(), asked: 0, due: now, failed: None };
        let step = lookup.ask(now, id);
        Some((lookup, step))
    }

    /// When the lookup must be woken if no datagram arrives first.
    pub fn due(&self) -> u64 {
        self.due
    }

    /// The next query for `name`, or the end where every one has been sent.
    fn ask(&mut self, now: u64, id: u16) -> Step {
        if self.asked == ROUNDS * self.servers.len() {
            return Step::Done(Err(match self.failed {
                Some(rcode) => Failure::ServerFailed(rcode),
                None => Failure::TimedOut,
            }));
        }
        let to = self.servers[self.asked % self.servers.len()];
        self.asked += 1;
        self.sent.push(Sent { id, to });
        self.due = now + WAIT_MS;
        Step::Ask { to, query: query(id, &self.name) }
    }

    /// The time is `now`: the newest query has had its [`WAIT_MS`] where it
    /// is due. `id` is for the query this may send.
    pub fn on_time(&mut self, now: u64, id: u16) -> Step {
        if now < self.due {
            return Step::Wait;
        }
        self.ask(now, id)
    }

    /// `msg` arrived at `now` from `port` of `from`. `id` is for the query
    /// this may send.
    pub fn on_datagram(&mut self, from: [u8; 4], port: u16, msg: &[u8], now: u64, id: u16) -> Step {
        let Some(carried) = msg.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
            return Step::Wait;
        };
        let Some(which) = self.sent.iter().position(|s| port == PORT && s.to == from && s.id == carried) else {
            return Step::Wait;
        };
        match read(msg, carried, &self.name, MAX_ALIASES - self.aliases) {
            Verdict::Stray(_) => Step::Wait,
            Verdict::Truncated => Step::Done(Err(Failure::Truncated)),
            Verdict::NoSuchName => Step::Done(Err(Failure::NoSuchName)),
            Verdict::TooManyAliases => Step::Done(Err(Failure::TooManyAliases)),
            Verdict::ServerFailed(rcode) => {
                self.failed = Some(rcode);
                self.sent.remove(which);
                // The next server is asked now, unless a newer query is still
                // waiting for its own answer.
                if which == self.sent.len() {
                    self.ask(now, id)
                } else {
                    Step::Wait
                }
            }
            Verdict::Answer(answer) if !answer.addrs.is_empty() => Step::Done(Ok(answer.addrs)),
            Verdict::Answer(answer) => match answer.aliases.last() {
                None => Step::Done(Err(Failure::NoAddress)),
                // The chain ends at a name this reply holds nothing for: that
                // name is asked in its own right (RFC 1034 §5.3.3), with every
                // query and server afresh.
                Some(last) => {
                    self.aliases += answer.aliases.len();
                    self.name = last.clone();
                    self.sent.clear();
                    self.asked = 0;
                    self.failed = None;
                    self.ask(now, id)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests;
