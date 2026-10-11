//! DHCP messages (RFC 2131 §2, RFC 2132, RFC 3396): the client's, built to one layout with its
//! options in one order, and a server's, read by `Reply::parse`'s checks in order.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_wire::ethernet::MacAddr;

use crate::{limits, Counter, HostName};

const COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const OPTIONS_AT: usize = 240;
const SNAME: core::ops::Range<usize> = 44..108;
const FILE: core::ops::Range<usize> = 108..236;
/// The codes the client reads from a server; every other code is skipped whatever it holds.
const READ: [u8; 12] = [1, 3, 6, 50, 51, 52, 53, 54, 58, 59, 61, 80];
/// What the client asks for, and nothing it does not use (RFC 7844 §3.6).
const PARAMETERS: [u8; 6] = [1, 3, 6, 51, 58, 59];
const OVERLOAD: u8 = 52;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Discover,
    Request,
    Decline,
}

/// One message the client sends; `bytes` decides which options it carries.
pub(crate) struct Build<'a> {
    pub kind: Kind,
    pub xid: u32,
    pub secs: u16,
    pub ciaddr: Ipv4Addr,
    pub server: Option<Ipv4Addr>,
    pub requested: Option<Ipv4Addr>,
    pub mac: MacAddr,
    pub client_id: &'a [u8],
    pub host_name: Option<&'a HostName>,
}

fn option(out: &mut Vec<u8>, code: u8, data: &[u8]) {
    out.push(code);
    out.push(u8::try_from(data.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(data);
}

impl Build<'_> {
    /// The payload: fixed fields, the cookie, the options in one order, END, and zero bytes to
    /// at least 300 in all (RFC 1542 §2.1).
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(limits::MIN_SENT);
        out.extend_from_slice(&[1, 1, 6, 0]);
        out.extend_from_slice(&self.xid.to_be_bytes());
        out.extend_from_slice(&self.secs.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&self.ciaddr.octets());
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&self.mac.0);
        out.resize(OPTIONS_AT.saturating_sub(COOKIE.len()), 0);
        out.extend_from_slice(&COOKIE);
        let kind = match self.kind {
            Kind::Discover => 1,
            Kind::Request => 3,
            Kind::Decline => 4,
        };
        option(&mut out, 53, &[kind]);
        if let Some(server) = self.server {
            option(&mut out, 54, &server.octets());
        }
        if let Some(requested) = self.requested {
            option(&mut out, 50, &requested.octets());
        }
        option(&mut out, 61, self.client_id);
        if self.kind != Kind::Decline {
            option(&mut out, 57, &limits::MAX_MESSAGE.to_be_bytes());
            option(&mut out, 55, &PARAMETERS);
            if let Some(name) = self.host_name {
                option(&mut out, 12, name.bytes());
            }
        }
        if self.kind == Kind::Discover {
            option(&mut out, 80, &[]);
        }
        out.push(255);
        if out.len() < limits::MIN_SENT {
            out.resize(limits::MIN_SENT, 0);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplyKind {
    Offer,
    Ack,
    Nak,
}

/// A server message that passed `parse`'s checks, its options joined per RFC 3396 §7 and each read
/// one checked for its length.
#[derive(Debug)]
pub(crate) struct Reply {
    pub kind: ReplyKind,
    pub xid: u32,
    pub yiaddr: Ipv4Addr,
    /// Fields that ended without END, which RFC 2131 §4.1 requires: accepted, since once every
    /// option is contained its absence loses nothing.
    pub no_end: u64,
    pub server: Option<Ipv4Addr>,
    pub lease: Option<u32>,
    pub mask: Option<Ipv4Addr>,
    pub routers: Vec<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub t1: Option<u32>,
    pub t2: Option<u32>,
    pub client_id: Option<Vec<u8>>,
    pub rapid: bool,
}

/// Walks one field, joining every instance of a read code in order; `true` when END closed it.
fn walk(field: &[u8], found: &mut BTreeMap<u8, Vec<u8>>, overload_here: bool) -> Result<bool, Counter> {
    let mut rest = field;
    loop {
        match rest {
            [] => return Ok(false),
            [0, after @ ..] => rest = after,
            [255, ..] => return Ok(true),
            [code, after @ ..] => {
                let (&len, data) = after.split_first().ok_or(Counter::OptionTruncated)?;
                let (value, next) = data.split_at_checked(usize::from(len)).ok_or(Counter::OptionTruncated)?;
                if *code == OVERLOAD && !overload_here {
                    return Err(Counter::OverloadInvalid);
                }
                if READ.contains(code) {
                    found.entry(*code).or_default().extend_from_slice(value);
                }
                rest = next;
            }
        }
    }
}

fn address(bytes: &[u8]) -> Option<Ipv4Addr> {
    bytes.first_chunk::<4>().map(|&a| Ipv4Addr::from(a))
}

fn number(bytes: &[u8]) -> Option<u32> {
    bytes.first_chunk::<4>().map(|&n| u32::from_be_bytes(n))
}

fn addresses(bytes: &[u8]) -> Vec<Ipv4Addr> {
    bytes.as_chunks::<4>().0.iter().map(|&a| Ipv4Addr::from(a)).collect()
}

impl Reply {
    pub fn parse(bytes: &[u8], mac: MacAddr) -> Result<Self, Counter> {
        let (fixed, options) = bytes.split_at_checked(OPTIONS_AT).ok_or(Counter::Truncated)?;
        let (&[op, htype, hlen, _, x0, x1, x2, x3], _) = fixed.split_first_chunk::<8>().ok_or(Counter::Truncated)?;
        if op != 2 {
            return Err(Counter::NotReply);
        }
        if (htype, hlen) != (1, 6) {
            return Err(Counter::HardwareType);
        }
        if fixed.get(28..34) != Some(&mac.0[..]) {
            return Err(Counter::ChaddrMismatch);
        }
        if fixed.get(236..240) != Some(&COOKIE[..]) {
            return Err(Counter::BootpReply);
        }
        let mut found = BTreeMap::new();
        let mut no_end = u64::from(!walk(options, &mut found, true)?);
        let overload = match found.get(&OVERLOAD).map(Vec::as_slice) {
            None => 0,
            Some(&[value @ 1..=3]) => value,
            Some(_) => return Err(Counter::OverloadInvalid),
        };
        for (bit, range) in [(1, FILE), (2, SNAME)] {
            if overload & bit != 0 {
                let field = fixed.get(range).ok_or(Counter::Truncated)?;
                no_end = no_end.saturating_add(u64::from(!walk(field, &mut found, false)?));
            }
        }
        for (code, value) in &found {
            let len = value.len();
            let fits = match code {
                53 => len == 1,
                1 | 50 | 51 | 54 | 58 | 59 => len == 4,
                3 | 6 => len > 0 && len.is_multiple_of(4),
                80 => len == 0,
                61 => len >= 2,
                _ => true,
            };
            if !fits {
                return Err(Counter::OptionLength);
            }
        }
        let kind = match found.get(&53).map(Vec::as_slice) {
            None => return Err(Counter::MessageTypeMissing),
            Some(&[2]) => ReplyKind::Offer,
            Some(&[5]) => ReplyKind::Ack,
            Some(&[6]) => ReplyKind::Nak,
            Some(&[1 | 3 | 4 | 7 | 8]) => return Err(Counter::WrongDirection),
            Some(&[9]) => return Err(Counter::Forcerenew),
            Some(_) => return Err(Counter::MessageTypeUnsupported),
        };
        let get = |code: u8| found.get(&code).map(Vec::as_slice);
        Ok(Self {
            kind,
            xid: u32::from_be_bytes([x0, x1, x2, x3]),
            yiaddr: fixed.get(16..20).and_then(address).unwrap_or(Ipv4Addr::UNSPECIFIED),
            no_end,
            server: get(54).and_then(address),
            lease: get(51).and_then(number),
            mask: get(1).and_then(address),
            routers: get(3).map(addresses).unwrap_or_default(),
            dns: get(6).map(addresses).unwrap_or_default(),
            t1: get(58).and_then(number),
            t2: get(59).and_then(number),
            client_id: get(61).map(<[u8]>::to_vec),
            rapid: get(80).is_some(),
        })
    }
}
