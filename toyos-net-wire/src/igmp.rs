//! IGMP (RFC 2236; RFC 9776, which obsoletes RFC 3376): every query version,
//! the version 1 and 2 reports and the leave, parsed; the version 2 messages
//! and the version 3 report, built.
//!
//! The checksum covers the whole IP payload, not only the first 8 bytes. A
//! query's version is its length and code (RFC 9776 §7.1): 8 bytes with code
//! 0 is version 1, 8 bytes otherwise version 2, 12 or more version 3, and 9 to
//! 11 is malformed. Its maximum response code is linear for versions 1 and 2
//! and floating-point only for version 3. Bytes past a version 2 message's 8,
//! or past a version 3 query's sources, are ignored. Parsing a version 3
//! report is not implemented.
//!
//! 224.0.0.1 is never reported: [`ReportGroup`] cannot hold it. Every IGMP
//! message is sent with TTL 1 and a Router Alert ([`datagram`]).

use core::net::Ipv4Addr;

use crate::checksum::{Accumulator, PseudoHeader, Sum};
use crate::emit::{be16x2, put, BuildError};
use crate::ipv4::{Form, Ipv4Builder, Ipv4Payload, Ipv4Source, MulticastAddr, Protocol, TrafficClass, Ttl, ROUTER_ALERT};

pub const HEADER_LEN: usize = 8;

reasons! {
    /// Why a message was refused, in the order the checks run.
    IgmpError {
        /// Fewer than 8 bytes.
        Truncated = "igmp.truncated", Malformed;
        /// A sum over the IP payload that is not 0xFFFF.
        Checksum = "igmp.checksum", Malformed;
        /// A query of 9 to 11 bytes.
        QueryLength = "igmp.query-length", Malformed;
        /// A query group that is neither 0.0.0.0 nor multicast.
        QueryGroup = "igmp.query-group", Malformed;
        /// Fewer source addresses than a version 3 query counts.
        QuerySourcesOverrun = "igmp.query-sources-overrun", Malformed;
        /// A report or leave whose group is not multicast.
        Group = "igmp.group", Malformed;
        /// A version 3 report, which a host has no use for.
        V3Report = "igmp.v3-report", Unsupported;
        /// Any other type (RFC 2236 §2.1).
        UnknownType = "igmp.unknown-type", Unsupported;
    }
}

/// A time in tenths of a second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Deciseconds(pub u16);

/// Which groups a query asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryGroup {
    General,
    Specific(MulticastAddr),
}

/// A version 3 query's fields beyond version 2's (RFC 9776 §4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3Query<'a> {
    /// S: routers suppress their timer updates.
    pub suppress_router_processing: bool,
    /// QRV, the querier's robustness variable.
    pub robustness: u8,
    /// QQIC, the querier's query interval code.
    pub interval_code: u8,
    sources: &'a [[u8; 4]],
}

impl<'a> V3Query<'a> {
    pub fn sources(&self) -> impl ExactSizeIterator<Item = Ipv4Addr> + 'a {
        self.sources.iter().map(|&octets| Ipv4Addr::from(octets))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryVersion<'a> {
    V1,
    V2,
    V3(V3Query<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Query<'a> {
    pub group: QueryGroup,
    pub max_response: Deciseconds,
    pub version: QueryVersion<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgmpMessage<'a> {
    Query(Query<'a>),
    V2Report(MulticastAddr),
    V1Report(MulticastAddr),
    Leave(MulticastAddr),
}

/// A version 3 maximum response code in tenths: linear below 128, and above
/// `(mant | 0x10) << (exp + 3)` (RFC 9776 §4.1.1).
fn v3_max_response(code: u8) -> Deciseconds {
    if code < 0x80 {
        Deciseconds(u16::from(code))
    } else {
        let exponent = code >> 4 & 7;
        Deciseconds(u16::from(code & 0x0F | 0x10) << 3 << exponent)
    }
}

/// A parsed message, borrowing the bytes it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IgmpPacket<'a> {
    bytes: &'a [u8],
    message: IgmpMessage<'a>,
}

impl<'a> IgmpPacket<'a> {
    /// Parses the message that is the whole of `bytes`: an IPv4 payload.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, IgmpError> {
        let (&[kind, code, _, _, g0, g1, g2, g3], rest) = bytes.split_first_chunk::<HEADER_LEN>().ok_or(IgmpError::Truncated)?;
        if !Sum::of(bytes).verifies() {
            return Err(IgmpError::Checksum);
        }
        let group = Ipv4Addr::new(g0, g1, g2, g3);
        let reported = || MulticastAddr::new(group).ok_or(IgmpError::Group);
        let message = match kind {
            0x11 => {
                let (version, max_response) = match (rest, rest.split_first_chunk::<4>()) {
                    ([], _) if code == 0 => (QueryVersion::V1, Deciseconds(100)),
                    ([], _) => (QueryVersion::V2, Deciseconds(u16::from(code))),
                    (_, Some((&[flags, interval_code, n0, n1], sources))) => {
                        let count = usize::from(u16::from_be_bytes([n0, n1]));
                        let sources = sources.as_chunks::<4>().0.get(..count).ok_or(IgmpError::QuerySourcesOverrun)?;
                        let query = V3Query {
                            suppress_router_processing: flags & 0x08 != 0,
                            robustness: flags & 0x07,
                            interval_code,
                            sources,
                        };
                        (QueryVersion::V3(query), v3_max_response(code))
                    }
                    (_, None) => return Err(IgmpError::QueryLength),
                };
                let group = match MulticastAddr::new(group) {
                    Some(group) => QueryGroup::Specific(group),
                    None if group.is_unspecified() => QueryGroup::General,
                    None => return Err(IgmpError::QueryGroup),
                };
                IgmpMessage::Query(Query { group, max_response, version })
            }
            0x16 => IgmpMessage::V2Report(reported()?),
            0x12 => IgmpMessage::V1Report(reported()?),
            0x17 => IgmpMessage::Leave(reported()?),
            0x22 => return Err(IgmpError::V3Report),
            _ => return Err(IgmpError::UnknownType),
        };
        Ok(Self { bytes, message })
    }

    pub const fn message(&self) -> IgmpMessage<'a> {
        self.message
    }

    /// The message as received.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// A group a report may name: never 224.0.0.1 (RFC 2236 §6; RFC 9776 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportGroup(MulticastAddr);

impl ReportGroup {
    pub fn new(group: MulticastAddr) -> Result<Self, BuildError> {
        if group == MulticastAddr::ALL_HOSTS {
            Err(BuildError::IgmpReportAllHosts)
        } else {
            Ok(Self(group))
        }
    }

    pub const fn get(self) -> MulticastAddr {
        self.0
    }
}

/// An IGMP message to build, which also knows where it is sent.
pub trait IgmpBody: Ipv4Payload {
    fn destination(&self) -> MulticastAddr;
}

/// The datagram an IGMP message travels in: to its destination, TTL 1,
/// atomic, with a Router Alert (RFC 2236 §2; RFC 9776 §4).
pub fn datagram<M: IgmpBody>(source: Ipv4Source, traffic_class: TrafficClass, message: M) -> Ipv4Builder<'static, M> {
    Ipv4Builder {
        source,
        destination: message.destination().get(),
        ttl: Ttl::LINK,
        traffic_class,
        form: Form::Atomic,
        options: ROUTER_ALERT,
        payload: message,
    }
}

/// The version 1 and 2 messages a host sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V2Kind {
    /// To the group (RFC 2236 §3).
    Report,
    /// To the group, while a version 1 querier is present (RFC 2236 §4).
    V1Report,
    /// To 224.0.0.2 (RFC 2236 §3).
    Leave,
}

/// A version 1 or 2 message to build: 8 bytes, maximum response 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V2Builder {
    pub kind: V2Kind,
    pub group: ReportGroup,
}

impl Ipv4Payload for V2Builder {
    fn protocol(&self) -> Protocol {
        Protocol::Igmp
    }

    fn length(&self, _room: usize) -> Result<usize, BuildError> {
        Ok(HEADER_LEN)
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let kind = match self.kind {
            V2Kind::Report => 0x16,
            V2Kind::V1Report => 0x12,
            V2Kind::Leave => 0x17,
        };
        let [g0, g1, g2, g3] = self.group.0.get().octets();
        let [c0, c1] = Sum::of(&[kind, 0, 0, 0, g0, g1, g2, g3]).checksum().to_be_bytes();
        put(out, [kind, 0, c0, c1, g0, g1, g2, g3]).map(|_| ())
    }
}

impl IgmpBody for V2Builder {
    fn destination(&self) -> MulticastAddr {
        match self.kind {
            V2Kind::Report | V2Kind::V1Report => self.group.0,
            V2Kind::Leave => MulticastAddr::ALL_ROUTERS,
        }
    }
}

/// A version 3 group record's type and sources (RFC 9776 §4.2.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordType<'a> {
    /// MODE_IS_INCLUDE (1): the answer to a group-and-source query.
    IsInclude(&'a [Ipv4Addr]),
    /// MODE_IS_EXCLUDE (2) with no sources: the answer to a query.
    IsExclude,
    /// CHANGE_TO_INCLUDE_MODE (3) with no sources: a leave.
    ToInclude,
    /// CHANGE_TO_EXCLUDE_MODE (4) with no sources: a join.
    ToExclude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupRecord<'a> {
    pub group: ReportGroup,
    pub record: RecordType<'a>,
}

impl GroupRecord<'_> {
    const fn sources(&self) -> &[Ipv4Addr] {
        match self.record {
            RecordType::IsInclude(sources) => sources,
            RecordType::IsExclude | RecordType::ToInclude | RecordType::ToExclude => &[],
        }
    }
}

/// A version 3 membership report (RFC 9776 §4.2), sent to 224.0.0.22.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3ReportBuilder<'a> {
    pub records: &'a [GroupRecord<'a>],
}

impl Ipv4Payload for V3ReportBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Igmp
    }

    fn length(&self, room: usize) -> Result<usize, BuildError> {
        let length = self.records.iter().try_fold(HEADER_LEN, |length, record| {
            record.sources().len().checked_mul(4)?.checked_add(8)?.checked_add(length)
        });
        length.filter(|&length| length <= room).ok_or(BuildError::IpTooLong)
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let count = u16::try_from(self.records.len()).map_err(|_| BuildError::IpTooLong)?;
        let (header, mut rest) = out.split_first_chunk_mut::<HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
        let mut sum = Accumulator::new();
        for record in self.records {
            let kind = match record.record {
                RecordType::IsInclude(_) => 1,
                RecordType::IsExclude => 2,
                RecordType::ToInclude => 3,
                RecordType::ToExclude => 4,
            };
            let sources = u16::try_from(record.sources().len()).map_err(|_| BuildError::IpTooLong)?;
            let [n0, n1] = sources.to_be_bytes();
            let [g0, g1, g2, g3] = record.group.0.get().octets();
            let fixed = [kind, 0, n0, n1, g0, g1, g2, g3];
            sum = sum.feed(&fixed);
            rest = put(rest, fixed)?;
            for source in record.sources() {
                sum = sum.feed(&source.octets());
                rest = put(rest, source.octets())?;
            }
        }
        let [a, b, n0, n1] = be16x2(0x2200, count);
        let [c0, c1] = sum.feed(&[a, b, 0, 0, 0, 0, n0, n1]).sum().checksum().to_be_bytes();
        put(header, [a, b, c0, c1, 0, 0, n0, n1]).map(|_| ())
    }
}

impl IgmpBody for V3ReportBuilder<'_> {
    fn destination(&self) -> MulticastAddr {
        MulticastAddr::IGMPV3_ROUTERS
    }
}
