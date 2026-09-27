//! IPv4 datagrams and their options (RFC 791 §3.1).
//!
//! The header length is checked before the checksum, because it decides what
//! the checksum covers, and a truncated fragment is refused as truncated
//! before anything asks whether it is a fragment. Bytes after the total length
//! are link padding and belong to no packet. IPv4 has no "no checksum" value:
//! 0x0000 is verified like any other.
//!
//! The option list is walked once at parse: a zero or one-byte length can
//! neither stall the walk nor overrun the area, and every option but Router
//! Alert is carried as its kind and data without interpretation, repeats and
//! unknown kinds included. End of Option List ends the walk; what follows it
//! is padding whatever it holds. The reserved flag bit means nothing: it is
//! ignored and kept in the bytes.
//!
//! A built datagram is atomic (DF set, identification 0, RFC 6864 §4.1) or
//! fragmentable with the identification, MF and offset its caller names. Its
//! options are padded with zeros to a word, its lengths and checksum are its
//! own, and its source can be neither a group address nor class E.

use core::net::Ipv4Addr;
use core::num::NonZeroU8;

use crate::checksum::{Accumulator, Checksum, PseudoHeader, Sum};
use crate::emit::{exact, put, put_slice, BuildError};
use crate::ethernet::{FrameBody, TxEtherType};

/// A header without options.
pub const MIN_HEADER_LEN: usize = 20;
/// The most option bytes a header holds.
pub const MAX_OPTIONS_LEN: usize = 40;
/// The largest datagram the 16-bit total length can describe.
pub const MAX_LEN: usize = 65535;

const ROUTER_ALERT_KIND: u8 = 0x94;

reasons! {
    /// Why a datagram was refused, in the order the checks run.
    Ipv4Error {
        /// Fewer than 20 bytes.
        Truncated = "ip.truncated", Malformed;
        /// A version other than 4.
        Version = "ip.version", Malformed;
        /// A header length below 5 words.
        HeaderLength = "ip.header-length", Malformed;
        /// Fewer bytes than the header length names.
        HeaderOverrun = "ip.header-overrun", Malformed;
        /// A total length shorter than the header.
        TotalLengthBelowHeader = "ip.total-length-below-header", Malformed;
        /// A total length beyond the bytes present.
        TotalLengthOverrun = "ip.total-length-overrun", Malformed;
        /// A header whose sum is not 0xFFFF.
        HeaderChecksum = "ip.header-checksum", Malformed;
        /// An option length below 2.
        OptionLength = "ip.option-length", Malformed;
        /// An option that runs past the options area.
        OptionOverrun = "ip.option-overrun", Malformed;
        /// A Router Alert whose length is not 4 (RFC 2113 §2.1).
        RouterAlertLength = "ip.router-alert-length", Malformed;
    }
}

/// A protocol number that is none of the four this stack parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OtherProtocol(u8);

impl OtherProtocol {
    /// `None` for ICMP, IGMP, TCP and UDP, which have their own variants.
    pub const fn new(number: u8) -> Option<Self> {
        match Protocol::from_number(number) {
            Protocol::Other(other) => Some(other),
            Protocol::Icmp | Protocol::Igmp | Protocol::Tcp | Protocol::Udp => None,
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

/// The protocol a datagram carries.
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

/// An address in 224.0.0.0/4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MulticastAddr(Ipv4Addr);

impl MulticastAddr {
    /// 224.0.0.1, every host on the link.
    pub const ALL_HOSTS: Self = Self(Ipv4Addr::new(224, 0, 0, 1));
    /// 224.0.0.2, every router on the link: where an IGMPv2 leave goes.
    pub const ALL_ROUTERS: Self = Self(Ipv4Addr::new(224, 0, 0, 2));
    /// 224.0.0.22, where IGMPv3 reports go.
    pub const IGMPV3_ROUTERS: Self = Self(Ipv4Addr::new(224, 0, 0, 22));

    /// `None` outside 224.0.0.0/4.
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

/// An address a datagram may be sent from: never multicast, broadcast or
/// class E (RFC 1122 §3.2.1.3). 0.0.0.0 is one, while an address is acquired.
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

/// A time to live a datagram can be sent with: never 0 (RFC 1122 §3.2.1.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ttl(NonZeroU8);

impl Ttl {
    /// What a unicast datagram is sent with.
    pub const DEFAULT: Self = Self(NonZeroU8::MIN.saturating_add(63));
    /// What an IGMP message is sent with: it never leaves the link.
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

/// A 6-bit differentiated services codepoint (RFC 2474 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dscp(u8);

impl Dscp {
    /// `None` above 63.
    pub const fn new(dscp: u8) -> Option<Self> {
        if dscp < 64 {
            Some(Self(dscp))
        } else {
            None
        }
    }

    pub const fn value(self) -> u8 {
        self.0
    }
}

/// The ECN codepoint (RFC 3168 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ecn {
    NotEct,
    Ect1,
    Ect0,
    Ce,
}

/// The type of service byte: DSCP and ECN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrafficClass(u8);

impl TrafficClass {
    /// DSCP 0 and not ECN-capable.
    pub const ZERO: Self = Self(0);

    pub const fn new(dscp: Dscp, ecn: Ecn) -> Self {
        let ecn = match ecn {
            Ecn::NotEct => 0,
            Ecn::Ect1 => 1,
            Ecn::Ect0 => 2,
            Ecn::Ce => 3,
        };
        Self(dscp.0 << 2 | ecn)
    }

    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    pub const fn byte(self) -> u8 {
        self.0
    }

    pub const fn dscp(self) -> Dscp {
        Dscp(self.0 >> 2)
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

/// A fragment offset in units of 8 bytes, 13 bits wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentOffset(u16);

impl FragmentOffset {
    pub const ZERO: Self = Self(0);

    /// `None` above 8191.
    pub const fn new(units: u16) -> Option<Self> {
        if units < 0x2000 {
            Some(Self(units))
        } else {
            None
        }
    }

    /// The offset in a flags-and-offset field.
    pub(crate) const fn from_field(field: u16) -> Self {
        Self(field & 0x1FFF)
    }

    pub const fn units(self) -> u16 {
        self.0
    }

    /// The offset in bytes.
    pub fn bytes(self) -> u32 {
        u32::from(self.0) << 3
    }
}

/// An option kind that is an option: never End of Option List (0), No
/// Operation (1) or Router Alert (148), which has its own variant.
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

    /// Whether fragmentation copies the option into every fragment.
    pub const fn copied(self) -> bool {
        self.0 & 0x80 != 0
    }

    /// The 2-bit option class.
    pub const fn class(self) -> u8 {
        self.0 >> 5 & 3
    }

    /// The 5-bit option number.
    pub const fn number(self) -> u8 {
        self.0 & 0x1F
    }
}

/// One option. NOP and End of Option List are layout, not options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ipv4Option<'a> {
    /// Router Alert (RFC 2113); 0 asks every router to examine the datagram.
    RouterAlert(u16),
    /// Record Route, Timestamp, Security, the source routes, Stream ID and
    /// every unknown kind: 0 to 38 bytes of data, uninterpreted.
    Other { kind: OptionKind, data: &'a [u8] },
}

impl Ipv4Option<'_> {
    /// Its length on the wire.
    const fn len(&self) -> usize {
        match self {
            Self::RouterAlert(_) => 4,
            Self::Other { data, .. } => data.len().saturating_add(2),
        }
    }
}

/// The options a datagram carries to ask routers to examine it: an IGMP message has it.
pub const ROUTER_ALERT: &[Ipv4Option<'static>] = &[Ipv4Option::RouterAlert(0)];

/// The first option in `area` and what follows it, or `None` at its end.
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

/// An options area every option of which parsed.
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

    /// The options in order, repeats included.
    pub fn iter(&self) -> impl Iterator<Item = Ipv4Option<'a>> {
        let mut rest = self.0;
        core::iter::from_fn(move || {
            let (option, after) = next_option(rest).ok().flatten()?;
            rest = after;
            Some(option)
        })
    }

    /// The options area as received, layout and padding included.
    pub const fn bytes(&self) -> &'a [u8] {
        self.0
    }
}

/// A parsed datagram, borrowing the bytes it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Packet<'a> {
    datagram: &'a [u8],
    header: &'a [u8; MIN_HEADER_LEN],
    options: Ipv4Options<'a>,
    payload: &'a [u8],
}

impl<'a> Ipv4Packet<'a> {
    /// Parses the datagram at the front of `bytes`; what follows its total
    /// length is link padding.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Ipv4Error> {
        let (&[version_ihl, _, l0, l1, ..], _) = bytes.split_first_chunk::<MIN_HEADER_LEN>().ok_or(Ipv4Error::Truncated)?;
        if version_ihl >> 4 != 4 {
            return Err(Ipv4Error::Version);
        }
        let header_len = usize::from(version_ihl & 0x0F) << 2;
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

    /// Meaningless in an atomic datagram (RFC 6864 §4.1).
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

    /// MF set or a nonzero offset: a piece of a larger datagram.
    pub const fn is_fragment(&self) -> bool {
        self.more_fragments() || self.fragment_offset().0 != 0
    }

    /// As received; 0 and 1 included.
    pub const fn ttl(&self) -> u8 {
        self.header[8]
    }

    pub const fn protocol(&self) -> Protocol {
        Protocol::from_number(self.header[9])
    }

    pub const fn checksum(&self) -> Checksum {
        Checksum::from_field(u16::from_be_bytes([self.header[10], self.header[11]]))
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

    /// Exactly the total length less the header.
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// The datagram as received, without link padding.
    pub const fn bytes(&self) -> &'a [u8] {
        self.datagram
    }

    /// The pseudo-header a transport checksum in this datagram covers.
    pub(crate) fn pseudo_header(&self, length: u16) -> PseudoHeader {
        PseudoHeader { source: self.source(), destination: self.destination(), protocol: self.protocol(), length }
    }
}

/// What a datagram carries.
pub trait Ipv4Payload {
    fn protocol(&self) -> Protocol;

    /// Its length in bytes, or its own refusal when it does not fit in `room`,
    /// the bytes a datagram has left after its header.
    fn length(&self, room: usize) -> Result<usize, BuildError>;

    /// Writes the payload into `out`, which is exactly [`Self::length`] bytes;
    /// `pseudo` is the pseudo-header of the datagram it is written into.
    fn write(&self, pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError>;
}

/// Bytes already in their protocol's form: a protocol this crate has no
/// builder for, or a fragment of another datagram.
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

/// How a datagram may be fragmented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// DF set, MF clear, offset 0, identification 0.
    Atomic,
    /// DF clear, with what the caller names.
    Fragmentable { identification: u16, more_fragments: bool, offset: FragmentOffset },
}

/// A datagram to build.
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
    /// Writes the datagram at the front of `out` and returns it.
    pub fn emit<'b>(&self, out: &'b mut [u8]) -> Result<&'b [u8], BuildError> {
        let packet = exact(out, self.length()?)?;
        self.write(packet)?;
        Ok(packet)
    }

    /// The header length: the options padded to a word, then 20 bytes.
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
