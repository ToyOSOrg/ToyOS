//! Ethernet II frames (RFC 894) with up to two IEEE 802.1Q/802.1ad tags.
//!
//! The frame check sequence never reaches this module. No minimum length is
//! enforced on receive: 60 bytes is a transmit rule, and virtual devices
//! deliver shorter frames. The body a parse yields includes any link padding;
//! the layer above trims by its own length field. A built frame is padded with
//! zeros to 60 bytes, never carries a tag, and has an EtherType its body
//! decides, so a length field or an undefined type cannot be expressed.

use crate::emit::{exact, put, BuildError};
use crate::ipv4::MulticastAddr;

/// Header bytes before the body of an untagged frame.
pub const HEADER_LEN: usize = 14;
/// The largest body a built frame carries: netd's device MTU is a 1,514-byte frame.
pub const MAX_BODY: usize = 1500;
/// A shorter built body is padded to this, making a 60-byte frame (RFC 894).
pub const MIN_BODY: usize = 46;

reasons! {
    /// Why a frame was refused, in the order the checks run.
    EthError {
        /// Fewer than 14 bytes.
        Truncated = "eth.truncated", Malformed;
        /// A group source address: IEEE 802 sources are individual.
        GroupSource = "eth.group-source", Malformed;
        /// A third tag.
        TooManyTags = "eth.too-many-tags", Unsupported;
        /// A tag identifier without its control field and next type field.
        TruncatedTag = "eth.truncated-tag", Malformed;
        /// A type field of 0–1500, an IEEE 802.3 length: there is no LLC client.
        LengthFrame = "eth.length-frame", Unsupported;
        /// A type field of 1501–1535, which means nothing.
        UndefinedTypeField = "eth.undefined-type-field", Malformed;
    }
}

/// A 48-bit IEEE 802 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

/// What an address names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacClass {
    Individual,
    /// A group address other than broadcast.
    Group,
    /// All ones, which is also a group address.
    Broadcast,
}

impl MacAddr {
    pub const BROADCAST: Self = Self([0xFF; 6]);
    /// All zeros: an ARP request's target, which names nobody.
    pub const ZERO: Self = Self([0; 6]);

    pub const fn class(self) -> MacClass {
        let [first, ..] = self.0;
        if first & 1 == 0 {
            MacClass::Individual
        } else if matches!(self.0, [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]) {
            MacClass::Broadcast
        } else {
            MacClass::Group
        }
    }

    /// The group address an IPv4 multicast group maps to: 01:00:5e and the
    /// group's low 23 bits (RFC 1112 §6.4), so 32 groups share each address.
    pub const fn multicast(group: MulticastAddr) -> Self {
        let [_, b, c, d] = group.get().octets();
        Self([0x01, 0x00, 0x5E, b & 0x7F, c, d])
    }
}

/// An individual address: an interface's own, or a frame's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IndividualMac(MacAddr);

impl IndividualMac {
    /// `None` for a group address.
    pub const fn new(mac: MacAddr) -> Option<Self> {
        match mac.class() {
            MacClass::Individual => Some(Self(mac)),
            MacClass::Group | MacClass::Broadcast => None,
        }
    }

    pub const fn get(self) -> MacAddr {
        self.0
    }
}

/// The link destination of an IPv4 datagram that is not unicast. A unicast
/// destination is resolved by ARP and never sent in a broadcast frame
/// (RFC 1122 §3.3.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupDestination {
    /// Limited broadcast, or the subnet's directed broadcast.
    Broadcast,
    Multicast(MulticastAddr),
}

impl GroupDestination {
    pub const fn mac(self) -> MacAddr {
        match self {
            Self::Broadcast => MacAddr::BROADCAST,
            Self::Multicast(group) => MacAddr::multicast(group),
        }
    }
}

/// A type field of 0x0600 or above that is neither IPv4 nor ARP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OtherEtherType(u16);

impl OtherEtherType {
    pub const fn value(self) -> u16 {
        self.0
    }
}

/// The EtherType of a received frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EtherType {
    Ipv4,
    Arp,
    /// IPv6 (0x86DD), LLDP, 0x9100 and every other type.
    Other(OtherEtherType),
}

impl EtherType {
    pub const fn value(self) -> u16 {
        match self {
            Self::Ipv4 => 0x0800,
            Self::Arp => 0x0806,
            Self::Other(other) => other.0,
        }
    }
}

/// The EtherTypes ToyOS sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxEtherType {
    Ipv4,
    Arp,
}

/// Which tag a tag protocol identifier announces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagProtocol {
    /// 0x8100, an IEEE 802.1Q customer tag.
    Customer,
    /// 0x88A8, an IEEE 802.1ad service tag.
    Service,
}

impl TagProtocol {
    const fn from_field(field: u16) -> Option<Self> {
        match field {
            0x8100 => Some(Self::Customer),
            0x88A8 => Some(Self::Service),
            _ => None,
        }
    }
}

/// One tag: its identifier and its 16-bit control field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VlanTag {
    protocol: TagProtocol,
    control: u16,
}

impl VlanTag {
    pub const fn protocol(self) -> TagProtocol {
        self.protocol
    }

    /// The 3-bit priority code point.
    pub const fn priority(self) -> u8 {
        let [high, _] = self.control.to_be_bytes();
        high >> 5
    }

    pub const fn drop_eligible(self) -> bool {
        self.control & 0x1000 != 0
    }

    /// The 12-bit VLAN identifier; 0 marks a priority-tagged frame.
    pub const fn vlan_id(self) -> u16 {
        self.control & 0x0FFF
    }
}

/// The tags a frame carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tags {
    Untagged,
    Single(VlanTag),
    Double { outer: VlanTag, inner: VlanTag },
}

/// A parsed frame, borrowing the bytes it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    header: &'a [u8],
    destination: MacAddr,
    source: IndividualMac,
    tags: Tags,
    ether_type: EtherType,
    body: &'a [u8],
}

impl<'a> Frame<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, EthError> {
        let (addresses, rest) = bytes.split_first_chunk::<12>().ok_or(EthError::Truncated)?;
        let (field, mut rest) = rest.split_first_chunk::<2>().ok_or(EthError::Truncated)?;
        let [d0, d1, d2, d3, d4, d5, s0, s1, s2, s3, s4, s5] = *addresses;
        let source = IndividualMac::new(MacAddr([s0, s1, s2, s3, s4, s5])).ok_or(EthError::GroupSource)?;
        let mut field = u16::from_be_bytes(*field);
        let mut tags = Tags::Untagged;
        while let Some(protocol) = TagProtocol::from_field(field) {
            if matches!(tags, Tags::Double { .. }) {
                return Err(EthError::TooManyTags);
            }
            let (tag, after) = rest.split_first_chunk::<4>().ok_or(EthError::TruncatedTag)?;
            let [c0, c1, t0, t1] = *tag;
            let tag = VlanTag { protocol, control: u16::from_be_bytes([c0, c1]) };
            tags = match tags {
                Tags::Untagged => Tags::Single(tag),
                Tags::Single(outer) => Tags::Double { outer, inner: tag },
                Tags::Double { .. } => return Err(EthError::TooManyTags),
            };
            field = u16::from_be_bytes([t0, t1]);
            rest = after;
        }
        let ether_type = match field {
            0x0000..=0x05DC => return Err(EthError::LengthFrame),
            0x05DD..=0x05FF => return Err(EthError::UndefinedTypeField),
            0x0800 => EtherType::Ipv4,
            0x0806 => EtherType::Arp,
            other => EtherType::Other(OtherEtherType(other)),
        };
        let (header, body) = bytes.split_at(bytes.len().saturating_sub(rest.len()));
        Ok(Self {
            header,
            destination: MacAddr([d0, d1, d2, d3, d4, d5]),
            source,
            tags,
            ether_type,
            body,
        })
    }

    pub const fn destination(&self) -> MacAddr {
        self.destination
    }

    pub const fn source(&self) -> IndividualMac {
        self.source
    }

    pub const fn tags(&self) -> Tags {
        self.tags
    }

    pub const fn ether_type(&self) -> EtherType {
        self.ether_type
    }

    /// Addresses, tags and the final type field, exactly as received.
    pub const fn header(&self) -> &'a [u8] {
        self.header
    }

    /// Everything after the final type field, link padding included.
    pub const fn body(&self) -> &'a [u8] {
        self.body
    }
}

/// What a frame carries: it names its own EtherType and writes itself.
pub trait FrameBody {
    const ETHER_TYPE: TxEtherType;

    /// The body's length in bytes, or why it cannot be built.
    fn length(&self) -> Result<usize, BuildError>;

    /// Writes the body into `out`, which is exactly [`Self::length`] bytes.
    fn write(&self, out: &mut [u8]) -> Result<(), BuildError>;
}

/// The addresses of a frame to build; the body decides its EtherType.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameBuilder {
    pub destination: MacAddr,
    pub source: IndividualMac,
}

impl FrameBuilder {
    /// Writes the frame at the front of `out` and returns it.
    pub fn emit<'b, B: FrameBody>(&self, body: &B, out: &'b mut [u8]) -> Result<&'b [u8], BuildError> {
        let body_len = body.length()?;
        if body_len > MAX_BODY {
            return Err(BuildError::EthBodyTooLong);
        }
        let frame = exact(out, HEADER_LEN.saturating_add(body_len.max(MIN_BODY)))?;
        let [d0, d1, d2, d3, d4, d5] = self.destination.0;
        let [s0, s1, s2, s3, s4, s5] = self.source.get().0;
        let [t0, t1] = match B::ETHER_TYPE {
            TxEtherType::Ipv4 => EtherType::Ipv4,
            TxEtherType::Arp => EtherType::Arp,
        }
        .value()
        .to_be_bytes();
        let rest = put(&mut *frame, [d0, d1, d2, d3, d4, d5, s0, s1, s2, s3, s4, s5, t0, t1])?;
        let (body_out, padding) = rest.split_at_mut_checked(body_len).ok_or(BuildError::BufferTooSmall)?;
        body.write(body_out)?;
        padding.fill(0);
        Ok(frame)
    }
}
