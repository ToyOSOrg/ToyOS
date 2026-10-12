//! A stub resolver's decisions (RFC 1035): the question it asks for a name's
//! IPv4 addresses, which datagram answers that question, what the answer says,
//! when the question is asked again, and how long an answer is kept and for
//! whom. Pure: `core` and `alloc`, no `unsafe`, no I/O, and no clock or
//! randomness of its own. The caller hands in the time, the draw of every
//! query's ID and every datagram with its source.
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
//! **IPv4 only.** `A` is the one type asked for, because the stack under netstack
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
//! **An answer is kept for the view that asked it, under the name it asked**
//! ([`Cache`]): a view is one program's, and no view is answered from another's,
//! so no program learns from the time a lookup takes which names another asked.
//! No alias target, additional or authority record is kept in its own right,
//! so a reply for one name plants nothing for another.
//!
//! **One clock a lookup.** Every TTL a lookup reads counts from its first
//! query's send, which is no later than any resolver sent any record it read,
//! so no record outlives what its own reply said. An answer is kept until then
//! plus the least TTL of the aliases followed and the address RRset, its TTLs
//! read at their lowest where they differ (RFC 2181 §5.2); a name's absence,
//! NXDOMAIN or NODATA (RFC 2308 §2.1, §2.2), until then plus the least of the
//! aliases that led to it and its zone's SOA's TTL and MINIMUM (§5), where the
//! authority section holds an SOA owned by the denied name or an ancestor of
//! it, and never where it holds none. A TTL is read unsigned (RFC 8767 §4) and
//! capped at [`KEEP_CAP_S`]; one of zero keeps nothing (RFC 1035 §4.1.3), and
//! neither does a failure.

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

/// Lookups in flight at once. Policy: each holds a socket and its buffers per
/// query waiting for its answer until it ends, and a client may start any
/// number of them.
pub const MAX_LOOKUPS: usize = 16;

/// The longest any answer is kept, in seconds, whatever its TTL says. Policy:
/// it bounds how long a wrong answer that passed every check, forged or for a
/// host that moved, outlives its correction, at a query an hour for each name
/// a program keeps asking.
pub const KEEP_CAP_S: u32 = 3_600;

/// The names one view keeps at once. Policy, against what one desktop session
/// resolves.
pub const VIEW_ENTRIES: usize = 128;

/// The views a [`Cache`] keeps at once. Policy, as [`VIEW_ENTRIES`]. An entry
/// is a name of at most 255 bytes and the addresses of one reply, so the two
/// counts bound the bytes.
pub const MAX_VIEWS: usize = 32;

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
const TYPE_SOA: u16 = 6;
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

    /// This name is `zone` or a name below it: `zone` is this one, or this one
    /// with labels taken off its front.
    fn within(&self, zone: &Name) -> bool {
        let mut at = 0;
        loop {
            if self.wire[at..].eq_ignore_ascii_case(&zone.wire) {
                return true;
            }
            match usize::from(self.wire[at]) {
                0 => return false,
                len => at += 1 + len,
            }
        }
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
    /// (RFC 2308 §2.1); and for how many seconds that may be kept, by the
    /// aliases followed to it and its zone's SOA, zero with no SOA.
    NoSuchName(u32),
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
    /// For how many seconds what this reply says may be kept: the least TTL
    /// among the aliases followed and the chain's last name's addresses; with
    /// neither, the denied name's SOA's, zero with no SOA.
    pub keep: u32,
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

/// The 16-bit field at `at` in `msg` (RFC 1035 §2.3.2), bounds-checked.
pub fn u16_at(msg: &[u8], at: usize) -> Result<u16, Stray> {
    Message { bytes: msg }.u16(at)
}

/// The name at `at` in `msg`, and where the bytes after it begin.
///
/// **The tree's one reader of a DNS name off the wire**: the resolver's
/// replies and `toyos-mdns`'s questions are both read through it, so a rule
/// fixed here is fixed at both trust boundaries.
pub fn name_at(msg: &[u8], at: usize) -> Result<(Name, usize), Stray> {
    Message { bytes: msg }.name(at)
}

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

    fn u32(&self, at: usize) -> Result<u32, Stray> {
        let b = self.slice(at, 4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
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
    /// Capped, as [`keep`] reads one.
    ttl: u32,
    data: usize,
    end: usize,
}

fn record(msg: &Message, at: usize) -> Result<Record, Stray> {
    let (owner, fixed) = msg.name(at)?;
    let rtype = msg.u16(fixed)?;
    let class = msg.u16(fixed + 2)?;
    let ttl = keep(msg.u32(fixed + 4)?);
    let len = usize::from(msg.u16(fixed + 8)?);
    let data = fixed + 10;
    msg.slice(data, len)?;
    Ok(Record { owner, rtype, class, ttl, data, end: data + len })
}

/// A TTL as seconds to keep: unsigned (RFC 8767 §4), and at most
/// [`KEEP_CAP_S`].
fn keep(ttl: u32) -> u32 {
    ttl.min(KEEP_CAP_S)
}

/// The aliases from `asked` along `cnames`, each with its TTL, or `None` for a
/// chain longer than `budget`.
fn chain<'n>(asked: &Name, cnames: &'n [(Name, Name, u32)], budget: usize) -> Option<Vec<(&'n Name, u32)>> {
    let mut chain: Vec<(&Name, u32)> = Vec::new();
    loop {
        let now = chain.last().map_or(asked, |(name, _)| *name);
        let Some((_, target, ttl)) = cnames.iter().find(|(owner, ..)| owner.same(now)) else { return Some(chain) };
        if chain.len() == budget {
            return None;
        }
        chain.push((target, *ttl));
    }
}

/// For how many seconds `denied`'s absence may be kept, by the `count` records
/// of the authority section at `at`: the least, over each SOA there owned by
/// `denied` or an ancestor of it, of its TTL and its MINIMUM (RFC 2308 §5);
/// zero where there is none.
fn absence(msg: &Message, mut at: usize, count: u16, denied: &Name) -> Result<u32, Stray> {
    let mut least: Option<u32> = None;
    for _ in 0..count {
        let r = record(msg, at)?;
        at = r.end;
        if r.class != CLASS_IN || r.rtype != TYPE_SOA {
            continue;
        }
        // MNAME and RNAME, then SERIAL, REFRESH, RETRY, EXPIRE and MINIMUM
        // (RFC 1035 §3.3.13).
        let (_, rname) = msg.name(r.data)?;
        let (_, fields) = msg.name(rname)?;
        if fields + 20 != r.end {
            return Err(Stray::Malformed("an SOA record whose data is not two names and five numbers"));
        }
        if !denied.within(&r.owner) {
            continue;
        }
        let soa = r.ttl.min(keep(msg.u32(fields + 16)?));
        least = Some(least.map_or(soa, |least| least.min(soa)));
    }
    Ok(least.unwrap_or(0))
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
    let rcode = (flags & RCODE) as u8;
    if rcode != 0 && rcode != NXDOMAIN {
        return Ok(Verdict::ServerFailed(rcode));
    }

    let mut cnames: Vec<(Name, Name, u32)> = Vec::new();
    let mut addrs: Vec<(Name, [u8; 4], u32)> = Vec::new();
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
                addrs.push((r.owner, addr, r.ttl));
            }
            TYPE_CNAME => {
                let (target, end) = msg.name(r.data)?;
                if end != r.end {
                    return Err(Stray::Malformed("a CNAME record whose data is not one name"));
                }
                // One alias per name (RFC 1034 §3.6.2, RFC 2181 §10.1): a
                // second leaves the chain with two ways to go.
                if cnames.iter().any(|(owner, ..)| owner.same(&r.owner)) {
                    return Err(Stray::Malformed("two CNAME records for one name"));
                }
                cnames.push((r.owner, target, r.ttl));
            }
            _ => {}
        }
    }
    let authority = msg.u16(8)?;

    let chain = chain(asked, &cnames, budget);
    if rcode == NXDOMAIN {
        // The name denied is the chain's last (RFC 2308 §2.1). A chain too
        // long to read keeps nothing, and the name is no less denied.
        let keep = match chain {
            Some(chain) => {
                let denied = chain.last().map_or(asked, |(name, _)| *name);
                chain.iter().map(|(_, ttl)| *ttl).fold(absence(msg, at, authority, denied)?, u32::min)
            }
            None => 0,
        };
        return Ok(Verdict::NoSuchName(keep));
    }
    let Some(chain) = chain else { return Ok(Verdict::TooManyAliases) };
    let owner = chain.last().map_or(asked, |(name, _)| *name);
    let mut found: Vec<[u8; 4]> = Vec::new();
    let mut least = chain.iter().map(|(_, ttl)| *ttl).min();
    for (_, addr, ttl) in addrs.iter().filter(|(name, ..)| name.same(owner)) {
        least = Some(least.map_or(*ttl, |least| least.min(*ttl)));
        if !found.contains(addr) {
            found.push(*addr);
        }
    }
    let keep = match least {
        Some(least) => least,
        // Neither an alias nor an address: the name has no `A` record (RFC
        // 2308 §2.2), and its absence is the SOA's to time.
        None => absence(msg, at, authority, asked)?,
    };
    Ok(Verdict::Answer(Answer { aliases: chain.into_iter().map(|(name, _)| name.clone()).collect(), addrs: found, keep }))
}

/// How a lookup ended without an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The name does not exist (RCODE 3).
    NoSuchName,
    /// The name exists and has no `A` record (RFC 2308 §2.2).
    NoAddress,
    /// No server answered, and a query that may have reached one was given
    /// its [`WAIT_MS`].
    TimedOut,
    /// No query reached a server ([`Lookup::on_unreached`]) and none is left
    /// to send at once: there is no answer to wait for.
    Unreachable,
    /// The answer did not fit a UDP reply (TC).
    Truncated,
    /// Every server that answered could not answer, the last with this RCODE.
    ServerFailed(u8),
    /// The alias chain is longer than [`MAX_ALIASES`].
    TooManyAliases,
}

/// One query of a lookup, as the caller tells it from the lookup's others.
///
/// The caller sends each query from a port of its own, so that no query waits
/// behind another's (a query to a server whose link address never resolves
/// stays queued), and names the query whose port a datagram arrived on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Asked(u32);

/// What the caller does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send `query` to port [`PORT`] of `to` from a port of its own, which
    /// datagrams for `asked` arrive on, then wait until [`Lookup::due`].
    Ask { asked: Asked, to: [u8; 4], query: Vec<u8> },
    /// Nothing to send: wait until [`Lookup::due`] or the next datagram.
    Wait,
    /// The lookup is over: the name's addresses, at least one, or why none.
    Done(Result<Vec<[u8; 4]>, Failure>),
}

/// One query sent for the name now asked.
#[derive(Clone, Copy, Debug)]
struct Sent {
    asked: Asked,
    id: u16,
    to: [u8; 4],
}

/// One name's lookup, from its first query to its answer.
///
/// **Each query carries an ID the caller draws for it**, from a source an
/// off-path sender cannot predict (RFC 5452 §9.2). Every call that may send a
/// query takes the draw, and calls it once for a query it sends and not at all
/// otherwise. A reply to
/// any query this lookup sent for the name now asked is read, so an answer
/// late past its query's wait still ends the lookup; it is read only on the
/// query's own port, with the query's ID, from the server the query went to.
///
/// **A wait is for an answer that can come.** When the newest query is
/// answered with a failure, or known or said not to have reached its server,
/// the next server is asked at once: RFC 1035 §4.2.1's interval of two to
/// five seconds is between repetitions of a query that may have been lost on
/// its way, and the same section has the other servers tried first. **The
/// interval is each server's**: one that has not answered is sent no second
/// query inside [`WAIT_MS`] of its last, or of the caller's last knowledge
/// that it was not reached, whatever is said of that query and whatever
/// another server answers; a server is its address, however often the list
/// names it. Where the next server cannot be asked yet, the lookup waits
/// until it can while any answer is still read, and ends at once only with
/// none.
///
/// **What the caller knows and what it was told are two calls.** A query the
/// caller itself knows never reached its server ([`Lookup::on_unreached`]) is
/// let go, and a lookup with no query left whose answer is read ends with
/// [`Failure::Unreachable`]. A report from the network
/// ([`Lookup::on_report`]) is anyone's word (RFC 8085 §5.2): it lets no query
/// go and ends nothing, so the answer to a query reported so is still read.
#[derive(Debug)]
pub struct Lookup {
    /// The name now asked: the one given, or the last alias followed.
    name: Name,
    /// Aliases followed so far, counted against [`MAX_ALIASES`].
    aliases: usize,
    /// Each server, and when it may be asked again: [`WAIT_MS`] after its
    /// last query or after it was last known not to be reached, and at once
    /// when it has answered any query, an older one's late failure included.
    /// An address named twice is one server: every place it has carries the
    /// same time.
    servers: Vec<([u8; 4], u64)>,
    /// Queries sent for `name`, oldest first; one answered with a server
    /// failure, or known not to have reached its server, is taken out.
    /// [`Lookup::waiting`] is this list, which holds a query while the
    /// lookup lasts.
    sent: Vec<Sent>,
    /// The [`Asked`] the next query gets: none is given twice in a lookup.
    next: u32,
    /// Queries sent for `name`, answered or not.
    asked: usize,
    /// When the newest query is given up on; or, where it is not waited for
    /// and the next server may not be asked yet, that server's turn.
    due: u64,
    /// The RCODE of the last server failure, which is what a lookup that runs
    /// out of queries reports if any server answered at all.
    failed: Option<u8>,
    /// When the first query was sent, which every TTL read counts from.
    began: u64,
    /// The least TTL of every record read so far, in seconds.
    keep: u32,
}

impl Lookup {
    /// A lookup of `name` through `servers`, and its first query, sent at
    /// `now` with the ID `id` draws. `None` when there is no server to ask.
    pub fn start(name: Name, servers: &[[u8; 4]], now: u64, id: impl FnOnce() -> u16) -> Option<(Self, Step)> {
        if servers.is_empty() {
            return None;
        }
        let mut lookup = Self {
            name,
            aliases: 0,
            servers: servers.iter().map(|server| (*server, now)).collect(),
            sent: Vec::new(),
            next: 0,
            asked: 0,
            due: now,
            failed: None,
            began: now,
            keep: KEEP_CAP_S,
        };
        let step = lookup.ask(now, id);
        Some((lookup, step))
    }

    /// When the lookup must be woken if no datagram arrives first.
    pub fn due(&self) -> u64 {
        self.due
    }

    /// Until when what the lookup ended with may be answered again without
    /// asking: its first query's send plus the least TTL of every record it
    /// read, and `None` where that is zero.
    pub fn keep_until(&self) -> Option<u64> {
        (self.keep > 0).then(|| self.began.saturating_add(u64::from(self.keep) * 1_000))
    }

    /// The queries whose answer is still read, oldest first. The port of any
    /// other query of this lookup can be let go.
    pub fn waiting(&self) -> impl Iterator<Item = Asked> + '_ {
        self.sent.iter().map(|s| s.asked)
    }

    /// The next server's query, if it may be asked at `now`; or the wait for
    /// the answers still read, until it may or the last query's wait is
    /// over; or the end.
    fn ask(&mut self, now: u64, id: impl FnOnce() -> u16) -> Step {
        let turn = self.asked % self.servers.len();
        let (to, free) = self.servers[turn];
        let spent = self.asked == ROUNDS * self.servers.len();
        if !spent && now >= free {
            self.free(to, now + WAIT_MS);
            self.asked += 1;
            let asked = Asked(self.next);
            self.next += 1;
            let id = id();
            self.sent.push(Sent { asked, id, to });
            self.due = now + WAIT_MS;
            return Step::Ask { asked, to, query: query(id, &self.name) };
        }
        if self.sent.is_empty() || (spent && now >= self.due) {
            // With no query left, each was answered with a failure or never
            // reached its server.
            let silence = if self.sent.is_empty() { Failure::Unreachable } else { Failure::TimedOut };
            return Step::Done(Err(self.failed.map_or(silence, Failure::ServerFailed)));
        }
        if !spent {
            // The newest query is not waited for: the next server's turn is.
            self.due = free;
        }
        Step::Wait
    }

    /// `server` may next be asked at `at`.
    fn free(&mut self, server: [u8; 4], at: u64) {
        for (_, free) in self.servers.iter_mut().filter(|(addr, _)| *addr == server) {
            *free = at;
        }
    }

    /// The time is `now`: the newest query has had its [`WAIT_MS`] where it
    /// is due. `id` draws the ID of the query this sends.
    pub fn on_time(&mut self, now: u64, id: impl FnOnce() -> u16) -> Step {
        if now < self.due {
            return Step::Wait;
        }
        self.ask(now, id)
    }

    /// The caller knows at `now`, of its own knowledge, that the query
    /// `asked` did not reach its server: it was never sent. Its answer is
    /// read no more. `id` draws the ID of the query this sends.
    pub fn on_unreached(&mut self, asked: Asked, now: u64, id: impl FnOnce() -> u16) -> Step {
        let Some(which) = self.sent.iter().position(|s| s.asked == asked) else {
            return Step::Wait;
        };
        let unreached = self.sent.remove(which);
        self.free(unreached.to, now + WAIT_MS);
        // A newer query still waits for its own answer.
        if which < self.sent.len() {
            return Step::Wait;
        }
        self.ask(now, id)
    }

    /// The network said at `now` that the query `asked` did not reach its
    /// server. Its answer is still read. `id` draws the ID of the query this
    /// sends.
    pub fn on_report(&mut self, asked: Asked, now: u64, id: impl FnOnce() -> u16) -> Step {
        // A newer query still waits for its own answer.
        if self.sent.last().is_none_or(|s| s.asked != asked) {
            return Step::Wait;
        }
        self.ask(now, id)
    }

    /// `msg` arrived at `now` from `port` of `from`, on the port `asked` was
    /// sent from. `id` draws the ID of the query this sends.
    pub fn on_datagram(
        &mut self,
        asked: Asked,
        from: [u8; 4],
        port: u16,
        msg: &[u8],
        now: u64,
        id: impl FnOnce() -> u16,
    ) -> Step {
        let Some(carried) = msg.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
            return Step::Wait;
        };
        let Some(which) =
            self.sent.iter().position(|s| s.asked == asked && port == PORT && s.to == from && s.id == carried)
        else {
            return Step::Wait;
        };
        match read(msg, carried, &self.name, MAX_ALIASES - self.aliases) {
            Verdict::Stray(_) => Step::Wait,
            Verdict::Truncated => Step::Done(Err(Failure::Truncated)),
            Verdict::NoSuchName(keep) => {
                self.keep = self.keep.min(keep);
                Step::Done(Err(Failure::NoSuchName))
            }
            Verdict::TooManyAliases => Step::Done(Err(Failure::TooManyAliases)),
            Verdict::ServerFailed(rcode) => {
                self.failed = Some(rcode);
                self.sent.remove(which);
                // It answered: its interval, which is for a query that may
                // have been lost, is over.
                self.free(from, now);
                // The next server is asked now, unless a newer query is still
                // waiting for its own answer.
                if which == self.sent.len() {
                    self.ask(now, id)
                } else {
                    Step::Wait
                }
            }
            Verdict::Answer(answer) => {
                self.keep = self.keep.min(answer.keep);
                match answer.aliases.last() {
                    _ if !answer.addrs.is_empty() => Step::Done(Ok(answer.addrs)),
                    None => Step::Done(Err(Failure::NoAddress)),
                    // The chain ends at a name this reply holds nothing for:
                    // that name is asked in its own right (RFC 1034 §5.3.3),
                    // with every query and server afresh.
                    Some(last) => {
                        self.aliases += answer.aliases.len();
                        self.name = last.clone();
                        self.sent.clear();
                        self.asked = 0;
                        self.failed = None;
                        for (_, free) in &mut self.servers {
                            *free = now;
                        }
                        self.ask(now, id)
                    }
                }
            }
        }
    }
}

/// Which emptying of a [`Cache`] a lookup started after: its answer is kept
/// only where no emptying came since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(u64);

/// One name a view keeps.
#[derive(Debug)]
struct Entry {
    name: Name,
    answer: Result<Vec<[u8; 4]>, Failure>,
    /// Answered before this time, and never at it or after.
    until: u64,
}

/// One program's names, least recently used first.
#[derive(Debug)]
struct View {
    key: Vec<u8>,
    entries: Vec<Entry>,
}

/// Every view's answers, and what empties them all at once.
///
/// **A view is named by bytes the caller vouches for**, and is answered only
/// from its own entries. A full view drops its expired entries, then its least
/// recently used one, so no view's lookup takes another's entry. With
/// [`MAX_VIEWS`] held a new view replaces another whole: one whose entries
/// have all expired, which tells its program nothing, and with none the least
/// recently used, live entries and all, whose program then finds its next
/// lookup asked (`issues/netstack-replaces-one-programs-cached-answers-to-keep-anothers.md`).
///
/// **An emptying is a new generation**: an answer whose lookup started under
/// an older one is not kept, so a lookup in flight across an emptying keeps
/// nothing.
#[derive(Debug, Default)]
pub struct Cache {
    /// Least recently used first.
    views: Vec<View>,
    generation: u64,
}

impl Cache {
    /// The generation a lookup starting now carries to [`Self::keep`].
    pub fn generation(&self) -> Generation {
        Generation(self.generation)
    }

    /// Forgets every answer of every view.
    pub fn empty(&mut self) {
        self.views.clear();
        self.generation += 1;
    }

    /// What `view` keeps for `name` at `now`, if it keeps anything still.
    pub fn answer(&mut self, view: &[u8], name: &Name, now: u64) -> Option<Result<Vec<[u8; 4]>, Failure>> {
        let at = self.views.iter().position(|kept| kept.key == view)?;
        let mut kept = self.views.remove(at);
        let answer = kept.entries.iter().position(|entry| entry.name.same(name)).and_then(|at| {
            let entry = kept.entries.remove(at);
            (now < entry.until).then(|| {
                let answer = entry.answer.clone();
                kept.entries.push(entry);
                answer
            })
        });
        self.views.push(kept);
        answer
    }

    /// Keeps for `view` what a lookup of `name` that started under `started`
    /// ended with, until `until`: addresses or the name's absence, and never a
    /// failure, an answer [`Lookup::keep_until`] gives no time, or one from
    /// before an emptying. A later answer for the name replaces an earlier one.
    pub fn keep(&mut self, view: &[u8], name: &Name, started: Generation, answer: Result<&[[u8; 4]], Failure>, until: Option<u64>, now: u64) {
        let answer = match answer {
            Ok(addrs) => Ok(addrs.to_vec()),
            Err(absent @ (Failure::NoSuchName | Failure::NoAddress)) => Err(absent),
            Err(_) => return,
        };
        let Some(until) = until.filter(|until| now < *until) else { return };
        if started != self.generation() {
            return;
        }
        let mut kept = match self.views.iter().position(|kept| kept.key == view) {
            Some(at) => self.views.remove(at),
            None => {
                if self.views.len() == MAX_VIEWS {
                    let spent = self.views.iter().position(|kept| kept.entries.iter().all(|entry| entry.until <= now));
                    self.views.remove(spent.unwrap_or(0));
                }
                View { key: view.to_vec(), entries: Vec::new() }
            }
        };
        kept.entries.retain(|entry| !entry.name.same(name));
        if kept.entries.len() == VIEW_ENTRIES {
            kept.entries.retain(|entry| now < entry.until);
        }
        if kept.entries.len() == VIEW_ENTRIES {
            kept.entries.remove(0);
        }
        kept.entries.push(Entry { name: name.clone(), answer, until });
        self.views.push(kept);
    }
}

#[cfg(test)]
mod tests;
