//! IPv4 datagrams and their options (RFC 791 §3.1).

use core::net::Ipv4Addr;
use core::num::NonZeroU8;

use crate::checksum::{Accumulator, PseudoHeader, Sum};
use crate::emit::{exact, put, put_slice, BuildError};
use crate::ethernet::{FrameBody, TxEtherType};

pub const MIN_HEADER_LEN: usize = 20;
pub const MAX_OPTIONS_LEN: usize = 40;
pub const MAX_LEN: usize = 65535;

const ROUTER_ALERT_KIND: u8 = 0x94;

reasons! {
    Ipv4Error {
        Truncated = "ip.truncated", Malformed;
        Version = "ip.version", Malformed;
        HeaderLength = "ip.header-length", Malformed;
        HeaderOverrun = "ip.header-overrun", Malformed;
        TotalLengthBelowHeader = "ip.total-length-below-header", Malformed;
        TotalLengthOverrun = "ip.total-length-overrun", Malformed;
        HeaderChecksum = "ip.header-checksum", Malformed;
        OptionLength = "ip.option-length", Malformed;
        OptionOverrun = "ip.option-overrun", Malformed;
        RouterAlertLength = "ip.router-alert-length", Malformed;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OtherProtocol(u8);

impl OtherProtocol {
    pub const fn value(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Icmp,
    Igmp,
    Tcp,
    Udp,
    Other(OtherProtocol),
}

impl Protocol {
    pub const fn from_number(number: u8) -> Self {
        match number {
            1 => Self::Icmp,
            2 => Self::Igmp,
            6 => Self::Tcp,
            17 => Self::Udp,
            other => Self::Other(OtherProtocol(other)),
        }
    }

    pub const fn number(self) -> u8 {
        match self {
            Self::Icmp => 1,
            Self::Igmp => 2,
            Self::Tcp => 6,
            Self::Udp => 17,
            Self::Other(other) => other.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MulticastAddr(Ipv4Addr);

impl MulticastAddr {
    pub const ALL_HOSTS: Self = Self(Ipv4Addr::new(224, 0, 0, 1));
    pub const ALL_ROUTERS: Self = Self(Ipv4Addr::new(224, 0, 0, 2));
    pub const IGMPV3_ROUTERS: Self = Self(Ipv4Addr::new(224, 0, 0, 22));

    pub const fn new(address: Ipv4Addr) -> Option<Self> {
        if address.is_multicast() {
            Some(Self(address))
        } else {
            None
        }
    }

    pub const fn get(self) -> Ipv4Addr {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv4Source(Ipv4Addr);

impl Ipv4Source {
    pub const fn new(address: Ipv4Addr) -> Result<Self, BuildError> {
        let [first, ..] = address.octets();
        if first >= 224 {
            Err(BuildError::IpInvalidSource)
        } else {
            Ok(Self(address))
        }
    }

    pub const fn get(self) -> Ipv4Addr {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ttl(NonZeroU8);

impl Ttl {
    pub const DEFAULT: Self = Self(NonZeroU8::MIN.saturating_add(63));
    pub const LINK: Self = Self(NonZeroU8::MIN);

    pub const fn new(ttl: u8) -> Result<Self, BuildError> {
        match NonZeroU8::new(ttl) {
            Some(ttl) => Ok(Self(ttl)),
            None => Err(BuildError::IpTtlZero),
        }
    }

    pub const fn get(self) -> u8 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ecn {
    NotEct,
    Ect1,
    Ect0,
    Ce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrafficClass(u8);

impl TrafficClass {
    pub const ZERO: Self = Self(0);

    pub const fn new(dscp: u8, ecn: Ecn) -> Option<Self> {
        if dscp > 63 {
            return None;
        }
        let ecn = match ecn {
            Ecn::NotEct => 0,
            Ecn::Ect1 => 1,
            Ecn::Ect0 => 2,
            Ecn::Ce => 3,
        };
        Some(Self(dscp << 2 | ecn))
    }

    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    pub const fn byte(self) -> u8 {
        self.0
    }

    pub const fn dscp(self) -> u8 {
        self.0 >> 2
    }

    pub const fn ecn(self) -> Ecn {
        match self.0 & 3 {
            0 => Ecn::NotEct,
            1 => Ecn::Ect1,
            2 => Ecn::Ect0,
            _ => Ecn::Ce,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentOffset(u16);

impl FragmentOffset {
    pub const ZERO: Self = Self(0);

    pub const fn new(units: u16) -> Option<Self> {
        if units < 0x2000 {
            Some(Self(units))
        } else {
            None
        }
    }

    pub(crate) const fn from_field(field: u16) -> Self {
        Self(field & 0x1FFF)
    }

    pub const fn units(self) -> u16 {
        self.0
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptionKind(u8);

impl OptionKind {
    pub const fn new(kind: u8) -> Option<Self> {
        match kind {
            0 | 1 | ROUTER_ALERT_KIND => None,
            kind => Some(Self(kind)),
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }

    pub const fn copied(self) -> bool {
        self.0 & 0x80 != 0
    }

    pub const fn class(self) -> u8 {
        self.0 >> 5 & 3
    }

    pub const fn number(self) -> u8 {
        self.0 & 0x1F
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ipv4Option<'a> {
    RouterAlert(u16),
    Other { kind: OptionKind, data: &'a [u8] },
}

impl Ipv4Option<'_> {
    const fn len(&self) -> usize {
        match self {
            Self::RouterAlert(_) => 4,
            Self::Other { data, .. } => data.len().saturating_add(2),
        }
    }
}

pub const ROUTER_ALERT: &[Ipv4Option<'static>] = &[Ipv4Option::RouterAlert(0)];

fn next_option(mut area: &[u8]) -> Result<Option<(Ipv4Option<'_>, &[u8])>, Ipv4Error> {
    loop {
        let Some((&kind, rest)) = area.split_first() else {
            return Ok(None);
        };
        match kind {
            0 => return Ok(None),
            1 => area = rest,
            _ => {
                let &length = rest.first().ok_or(Ipv4Error::OptionOverrun)?;
                if length < 2 {
                    return Err(Ipv4Error::OptionLength);
                }
                let (option, after) = area.split_at_checked(usize::from(length)).ok_or(Ipv4Error::OptionOverrun)?;
                let (_, data) = option.split_first_chunk::<2>().ok_or(Ipv4Error::OptionLength)?;
                let option = match OptionKind::new(kind) {
                    Some(kind) => Ipv4Option::Other { kind, data },
                    None => {
                        let &[a, b] = <&[u8; 2]>::try_from(data).map_err(|_| Ipv4Error::RouterAlertLength)?;
                        Ipv4Option::RouterAlert(u16::from_be_bytes([a, b]))
                    }
                };
                return Ok(Some((option, after)));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Options<'a>(&'a [u8]);

impl<'a> Ipv4Options<'a> {
    fn parse(area: &'a [u8]) -> Result<Self, Ipv4Error> {
        let mut rest = area;
        while let Some((_, after)) = next_option(rest)? {
            rest = after;
        }
        Ok(Self(area))
    }

    pub fn iter(&self) -> impl Iterator<Item = Ipv4Option<'a>> {
        let mut rest = self.0;
        core::iter::from_fn(move || {
            let (option, after) = next_option(rest).ok().flatten()?;
            rest = after;
            Some(option)
        })
    }

    pub const fn bytes(&self) -> &'a [u8] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Packet<'a> {
    datagram: &'a [u8],
    header: &'a [u8; MIN_HEADER_LEN],
    options: Ipv4Options<'a>,
    payload: &'a [u8],
}

impl<'a> Ipv4Packet<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Ipv4Error> {
        let (&[version_ihl, _, l0, l1, ..], _) = bytes.split_first_chunk::<MIN_HEADER_LEN>().ok_or(Ipv4Error::Truncated)?;
        if version_ihl >> 4 != 4 {
            return Err(Ipv4Error::Version);
        }
        let header_len = usize::from(version_ihl & 0x0F) << 2;
        // Before the checksum: the header length decides what it covers.
        if header_len < MIN_HEADER_LEN {
            return Err(Ipv4Error::HeaderLength);
        }
        let (full_header, _) = bytes.split_at_checked(header_len).ok_or(Ipv4Error::HeaderOverrun)?;
        let total = usize::from(u16::from_be_bytes([l0, l1]));
        if total < header_len {
            return Err(Ipv4Error::TotalLengthBelowHeader);
        }
        let (datagram, _) = bytes.split_at_checked(total).ok_or(Ipv4Error::TotalLengthOverrun)?;
        if !Sum::of(full_header).verifies() {
            return Err(Ipv4Error::HeaderChecksum);
        }
        let (header, options) = full_header.split_first_chunk::<MIN_HEADER_LEN>().ok_or(Ipv4Error::Truncated)?;
        let options = Ipv4Options::parse(options)?;
        let (_, payload) = datagram.split_at_checked(header_len).ok_or(Ipv4Error::TotalLengthBelowHeader)?;
        Ok(Self { datagram, header, options, payload })
    }

    pub fn header_len(&self) -> usize {
        self.datagram.len().saturating_sub(self.payload.len())
    }

    pub const fn traffic_class(&self) -> TrafficClass {
        TrafficClass(self.header[1])
    }

    pub const fn total_length(&self) -> u16 {
        u16::from_be_bytes([self.header[2], self.header[3]])
    }

    pub const fn identification(&self) -> u16 {
        u16::from_be_bytes([self.header[4], self.header[5]])
    }

    pub const fn dont_fragment(&self) -> bool {
        self.header[6] & 0x40 != 0
    }

    pub const fn more_fragments(&self) -> bool {
        self.header[6] & 0x20 != 0
    }

    pub const fn fragment_offset(&self) -> FragmentOffset {
        FragmentOffset::from_field(u16::from_be_bytes([self.header[6], self.header[7]]))
    }

    pub const fn is_fragment(&self) -> bool {
        self.more_fragments() || self.fragment_offset().0 != 0
    }

    pub const fn ttl(&self) -> u8 {
        self.header[8]
    }

    pub const fn protocol(&self) -> Protocol {
        Protocol::from_number(self.header[9])
    }

    pub const fn source(&self) -> Ipv4Addr {
        Ipv4Addr::new(self.header[12], self.header[13], self.header[14], self.header[15])
    }

    pub const fn destination(&self) -> Ipv4Addr {
        Ipv4Addr::new(self.header[16], self.header[17], self.header[18], self.header[19])
    }

    pub const fn options(&self) -> Ipv4Options<'a> {
        self.options
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    pub const fn bytes(&self) -> &'a [u8] {
        self.datagram
    }

    pub(crate) fn pseudo_header(&self, length: u16) -> PseudoHeader {
        PseudoHeader { source: self.source(), destination: self.destination(), protocol: self.protocol(), length }
    }
}

pub trait Ipv4Payload {
    fn protocol(&self) -> Protocol;

    fn length(&self, room: usize) -> Result<usize, BuildError>;

    fn write(&self, pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError>;
}

/// Bytes already in their protocol's form: an unassigned protocol's, or a fragment's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawPayload<'a> {
    pub protocol: Protocol,
    pub bytes: &'a [u8],
}

impl Ipv4Payload for RawPayload<'_> {
    fn protocol(&self) -> Protocol {
        self.protocol
    }

    fn length(&self, _room: usize) -> Result<usize, BuildError> {
        Ok(self.bytes.len())
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        put_slice(out, self.bytes).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Identification 0: RFC 6864 §4.1 gives it no meaning, and zero leaks no counter.
    Atomic,
    Fragmentable { identification: u16, more_fragments: bool, offset: FragmentOffset },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Builder<'a, P> {
    pub source: Ipv4Source,
    pub destination: Ipv4Addr,
    pub ttl: Ttl,
    pub traffic_class: TrafficClass,
    pub form: Form,
    pub options: &'a [Ipv4Option<'a>],
    pub payload: P,
}

impl<P: Ipv4Payload> Ipv4Builder<'_, P> {
    pub fn emit<'b>(&self, out: &'b mut [u8]) -> Result<&'b [u8], BuildError> {
        let packet = exact(out, self.length()?)?;
        self.write(packet)?;
        Ok(packet)
    }

    fn header_len(&self) -> Result<usize, BuildError> {
        let options = self.options.iter().try_fold(0usize, |sum, option| sum.checked_add(option.len()));
        match options {
            Some(len) if len <= MAX_OPTIONS_LEN => Ok(MIN_HEADER_LEN.saturating_add(len.next_multiple_of(4))),
            _ => Err(BuildError::IpOptionsTooLong),
        }
    }
}

impl<P: Ipv4Payload> FrameBody for Ipv4Builder<'_, P> {
    const ETHER_TYPE: TxEtherType = TxEtherType::Ipv4;

    fn length(&self) -> Result<usize, BuildError> {
        let header_len = self.header_len()?;
        let room = MAX_LEN.saturating_sub(header_len);
        let payload = self.payload.length(room)?;
        if payload > room {
            return Err(BuildError::IpTooLong);
        }
        Ok(header_len.saturating_add(payload))
    }

    fn write(&self, out: &mut [u8]) -> Result<(), BuildError> {
        let header_len = self.header_len()?;
        let total = u16::try_from(out.len()).map_err(|_| BuildError::IpTooLong)?;
        let (header, payload) = out.split_at_mut_checked(header_len).ok_or(BuildError::BufferTooSmall)?;
        let (_, mut options) = header.split_first_chunk_mut::<MIN_HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
        for option in self.options {
            options = match option {
                Ipv4Option::RouterAlert(value) => {
                    let [v0, v1] = value.to_be_bytes();
                    put(options, [ROUTER_ALERT_KIND, 4, v0, v1])?
                }
                Ipv4Option::Other { kind, data } => {
                    let len = u8::try_from(option.len()).map_err(|_| BuildError::IpOptionsTooLong)?;
                    put_slice(put(options, [kind.0, len])?, data)?
                }
            };
        }
        options.fill(0);
        let (identification, flags) = match self.form {
            Form::Atomic => (0, 0x4000),
            Form::Fragmentable { identification, more_fragments, offset } => {
                (identification, if more_fragments { 0x2000 | offset.0 } else { offset.0 })
            }
        };
        let ihl = u8::try_from(header_len >> 2).map_err(|_| BuildError::IpOptionsTooLong)?;
        let [t0, t1] = total.to_be_bytes();
        let [i0, i1] = u16::to_be_bytes(identification);
        let [f0, f1] = u16::to_be_bytes(flags);
        let [s0, s1, s2, s3] = self.source.0.octets();
        let [d0, d1, d2, d3] = self.destination.octets();
        let protocol = self.payload.protocol();
        let mut bytes = [
            0x40 | ihl,
            self.traffic_class.0,
            t0,
            t1,
            i0,
            i1,
            f0,
            f1,
            self.ttl.get(),
            protocol.number(),
            0,
            0,
            s0,
            s1,
            s2,
            s3,
            d0,
            d1,
            d2,
            d3,
        ];
        let written = header.get(MIN_HEADER_LEN..).ok_or(BuildError::BufferTooSmall)?;
        let [c0, c1] = Accumulator::new().feed(&bytes).feed(written).sum().checksum().to_be_bytes();
        bytes[10] = c0;
        bytes[11] = c1;
        put(header, bytes)?;
        let length = u16::try_from(payload.len()).map_err(|_| BuildError::IpTooLong)?;
        let pseudo = PseudoHeader { source: self.source.0, destination: self.destination, protocol, length };
        self.payload.write(&pseudo, payload)
    }
}
