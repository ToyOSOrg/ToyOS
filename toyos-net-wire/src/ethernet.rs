//! Ethernet II frames (RFC 894) with up to two IEEE 802.1Q/802.1ad tags; the body a parse yields keeps its link padding.

use crate::emit::{exact, put, BuildError};
use crate::ipv4::MulticastAddr;

pub const HEADER_LEN: usize = 14;
pub const MAX_BODY: usize = 1500;
pub const MIN_BODY: usize = 46;

reasons! {
    EthError {
        Truncated = "eth.truncated", Malformed;
        GroupSource = "eth.group-source", Malformed;
        TooManyTags = "eth.too-many-tags", Unsupported;
        TruncatedTag = "eth.truncated-tag", Malformed;
        LengthFrame = "eth.length-frame", Unsupported;
        UndefinedTypeField = "eth.undefined-type-field", Malformed;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacClass {
    Individual,
    Group,
    Broadcast,
}

impl MacAddr {
    pub const BROADCAST: Self = Self([0xFF; 6]);
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

    /// RFC 1112 §6.4: 01:00:5e and the group's low 23 bits.
    pub const fn multicast(group: MulticastAddr) -> Self {
        let [_, b, c, d] = group.get().octets();
        Self([0x01, 0x00, 0x5E, b & 0x7F, c, d])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IndividualMac(MacAddr);

impl IndividualMac {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OtherEtherType(u16);

impl OtherEtherType {
    pub const fn value(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EtherType {
    Ipv4,
    Arp,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxEtherType {
    Ipv4,
    Arp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagProtocol {
    Customer,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VlanTag {
    protocol: TagProtocol,
    control: u16,
}

impl VlanTag {
    pub const fn protocol(self) -> TagProtocol {
        self.protocol
    }

    pub const fn priority(self) -> u8 {
        let [high, _] = self.control.to_be_bytes();
        high >> 5
    }

    pub const fn drop_eligible(self) -> bool {
        self.control & 0x1000 != 0
    }

    pub const fn vlan_id(self) -> u16 {
        self.control & 0x0FFF
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tags {
    Untagged,
    Single(VlanTag),
    Double { outer: VlanTag, inner: VlanTag },
}

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

    pub const fn header(&self) -> &'a [u8] {
        self.header
    }

    pub const fn body(&self) -> &'a [u8] {
        self.body
    }
}

/// Crate-private: nothing outside can implement its own body or call `length`/`write` directly, so a frame's bytes and their length always come from `emit`'s own exact-length write.
pub(crate) trait FrameBody {
    const ETHER_TYPE: TxEtherType;

    fn length(&self) -> Result<usize, BuildError>;

    fn write(&self, out: &mut [u8]) -> Result<(), BuildError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameBuilder {
    pub destination: MacAddr,
    pub source: IndividualMac,
}

impl FrameBuilder {
    #[allow(private_bounds)]
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
