//! IGMP (RFC 2236, RFC 9776).

use core::net::Ipv4Addr;

use crate::checksum::{Accumulator, PseudoHeader, Sum};
use crate::emit::{be16x2, put, BuildError};
use crate::ipv4::{sealed, Ipv4Builder, Ipv4Payload, Ipv4Source, MulticastAddr, Protocol, TrafficClass, Ttl, ROUTER_ALERT};

pub const HEADER_LEN: usize = 8;

reasons! {
    IgmpError {
        Truncated = "igmp.truncated", Malformed;
        Checksum = "igmp.checksum", Malformed;
        QueryLength = "igmp.query-length", Malformed;
        QueryGroup = "igmp.query-group", Malformed;
        QuerySourcesOverrun = "igmp.query-sources-overrun", Malformed;
        Group = "igmp.group", Malformed;
        V3Report = "igmp.v3-report", Unsupported;
        UnknownType = "igmp.unknown-type", Unsupported;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Deciseconds(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryGroup {
    General,
    Specific(MulticastAddr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3Query<'a> {
    pub suppress_router_processing: bool,
    pub robustness: u8,
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

fn v3_max_response(code: u8) -> Deciseconds {
    if code < 0x80 {
        Deciseconds(u16::from(code))
    } else {
        let exponent = code >> 4 & 7;
        Deciseconds(u16::from(code & 0x0F | 0x10) << 3 << exponent)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IgmpPacket<'a> {
    bytes: &'a [u8],
    message: IgmpMessage<'a>,
}

impl<'a> IgmpPacket<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, IgmpError> {
        let (&[kind, code, _, _, g0, g1, g2, g3], rest) = bytes.split_first_chunk::<HEADER_LEN>().ok_or(IgmpError::Truncated)?;
        if !Sum::of(bytes).verifies() {
            return Err(IgmpError::Checksum);
        }
        let group = Ipv4Addr::new(g0, g1, g2, g3);
        let reported = || MulticastAddr::new(group).ok_or(IgmpError::Group);
        let message = match kind {
            0x11 => {
                let v3 = match (rest, rest.split_first_chunk::<4>()) {
                    ([], _) => None,
                    (_, Some(v3)) => Some(v3),
                    (_, None) => return Err(IgmpError::QueryLength),
                };
                let group = match MulticastAddr::new(group) {
                    Some(group) => QueryGroup::Specific(group),
                    None if group.is_unspecified() => QueryGroup::General,
                    None => return Err(IgmpError::QueryGroup),
                };
                let (version, max_response) = match v3 {
                    None if code == 0 => (QueryVersion::V1, Deciseconds(100)),
                    None => (QueryVersion::V2, Deciseconds(u16::from(code))),
                    Some((&[flags, interval_code, n0, n1], sources)) => {
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

    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

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

pub trait IgmpBody: Ipv4Payload {
    fn destination(&self) -> MulticastAddr;
}

pub fn datagram<M: IgmpBody>(source: Ipv4Source, traffic_class: TrafficClass, message: M) -> Ipv4Builder<'static, M> {
    Ipv4Builder {
        source,
        destination: message.destination().get(),
        ttl: Ttl::LINK,
        traffic_class,
        options: ROUTER_ALERT,
        payload: message,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V2Kind {
    Report,
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V2Builder {
    pub kind: V2Kind,
    pub group: ReportGroup,
}

impl sealed::Sealed for V2Builder {}

impl Ipv4Payload for V2Builder {
    fn protocol(&self) -> Protocol {
        Protocol::Igmp
    }

    fn length(&self, _header_len: usize) -> Result<usize, BuildError> {
        Ok(HEADER_LEN)
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let kind = match self.kind {
            V2Kind::Report => 0x16,
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
            V2Kind::Report => self.group.0,
            V2Kind::Leave => MulticastAddr::ALL_ROUTERS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordType {
    IsExclude,
    ToInclude,
    ToExclude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupRecord {
    pub group: ReportGroup,
    pub record: RecordType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3ReportBuilder<'a> {
    pub records: &'a [GroupRecord],
}

impl sealed::Sealed for V3ReportBuilder<'_> {}

impl Ipv4Payload for V3ReportBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Igmp
    }

    fn length(&self, _header_len: usize) -> Result<usize, BuildError> {
        Ok(HEADER_LEN.saturating_add(self.records.len().saturating_mul(8)))
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let count = u16::try_from(self.records.len()).map_err(|_| BuildError::IpTooLong)?;
        let (header, mut rest) = out.split_first_chunk_mut::<HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
        let mut sum = Accumulator::new();
        for record in self.records {
            let kind = match record.record {
                RecordType::IsExclude => 2,
                RecordType::ToInclude => 3,
                RecordType::ToExclude => 4,
            };
            let [g0, g1, g2, g3] = record.group.0.get().octets();
            let fixed = [kind, 0, 0, 0, g0, g1, g2, g3];
            sum = sum.feed(&fixed);
            rest = put(rest, fixed)?;
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
