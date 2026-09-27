//! UDP datagrams (RFC 768).
//!
//! The datagram is the first length-field bytes of the IPv4 payload; anything
//! after them is not part of it and is not checksummed. A checksum of 0x0000
//! means none was computed and the datagram is accepted unchecked (RFC 1122
//! §4.1.3.4); any other value, 0xFFFF included, must verify over the
//! pseudo-header with the length field as its length. Source port 0 means
//! "no source port"; destination port 0 is malformed.
//!
//! A built datagram always carries a checksum, and one that computes to 0x0000
//! is sent as 0xFFFF: there is no checksumless mode.

use crate::checksum::{Checksum, PseudoHeader};
use crate::emit::{be16x2, put, put_slice, BuildError};
use crate::ipv4::{Ipv4Packet, Ipv4Payload, Protocol};
use crate::Port;

pub const HEADER_LEN: usize = 8;
/// The largest datagram the 16-bit length field can describe.
pub const MAX_LEN: usize = 65535;

reasons! {
    /// Why a datagram was refused, in the order the checks run.
    UdpError {
        /// Fewer than 8 bytes.
        Truncated = "udp.truncated", Malformed;
        /// A length field below 8.
        LengthBelowHeader = "udp.length-below-header", Malformed;
        /// A length field beyond the IPv4 payload.
        LengthOverrun = "udp.length-overrun", Malformed;
        /// A nonzero checksum that does not verify.
        Checksum = "udp.checksum", Malformed;
        /// Destination port 0, on which nothing can listen.
        DestinationPortZero = "udp.destination-port-zero", Malformed;
    }
}

/// UDP's checksum field, where 0x0000 means none was computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpChecksum {
    Absent,
    Present(Checksum),
}

impl UdpChecksum {
    pub const fn from_field(field: u16) -> Self {
        match field {
            0 => Self::Absent,
            field => Self::Present(Checksum::from_field(field)),
        }
    }

    /// The field's value: a computed 0x0000 is written 0xFFFF.
    pub const fn field(self) -> u16 {
        match self {
            Self::Absent => 0,
            Self::Present(checksum) => match checksum.value() {
                0 => 0xFFFF,
                value => value,
            },
        }
    }

    /// The field after a covered word changes (RFC 1624): a datagram without a
    /// checksum keeps none.
    #[must_use]
    pub fn replace(self, old: [u8; 2], new: [u8; 2]) -> Self {
        match self {
            Self::Absent => Self::Absent,
            Self::Present(checksum) => Self::Present(checksum.replace(old, new)),
        }
    }
}

/// A parsed datagram, borrowing the bytes it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    header: &'a [u8; HEADER_LEN],
    source: Option<Port>,
    destination: Port,
    payload: &'a [u8],
}

impl<'a> UdpDatagram<'a> {
    /// Parses the datagram `ip` carries, verified against `ip`'s addresses.
    pub fn parse(ip: &Ipv4Packet<'a>) -> Result<Self, UdpError> {
        let bytes = ip.payload();
        let (&[s0, s1, d0, d1, l0, l1, c0, c1], _) = bytes.split_first_chunk::<HEADER_LEN>().ok_or(UdpError::Truncated)?;
        let length = u16::from_be_bytes([l0, l1]);
        if usize::from(length) < HEADER_LEN {
            return Err(UdpError::LengthBelowHeader);
        }
        let (datagram, _) = bytes.split_at_checked(usize::from(length)).ok_or(UdpError::LengthOverrun)?;
        if UdpChecksum::from_field(u16::from_be_bytes([c0, c1])) != UdpChecksum::Absent
            && !ip.pseudo_header(length).accumulator().feed(datagram).sum().verifies()
        {
            return Err(UdpError::Checksum);
        }
        let destination = Port::new(u16::from_be_bytes([d0, d1])).ok_or(UdpError::DestinationPortZero)?;
        let (header, payload) = datagram.split_first_chunk::<HEADER_LEN>().ok_or(UdpError::Truncated)?;
        Ok(Self { header, source: Port::new(u16::from_be_bytes([s0, s1])), destination, payload })
    }

    /// `None` when the sender gave no source port.
    pub const fn source_port(&self) -> Option<Port> {
        self.source
    }

    pub const fn destination_port(&self) -> Port {
        self.destination
    }

    pub const fn checksum(&self) -> UdpChecksum {
        UdpChecksum::from_field(u16::from_be_bytes([self.header[6], self.header[7]]))
    }

    /// The 8 header bytes as received.
    pub const fn header(&self) -> &'a [u8; HEADER_LEN] {
        self.header
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// A datagram to build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpBuilder<'a> {
    pub source: Port,
    pub destination: Port,
    pub data: &'a [u8],
}

impl Ipv4Payload for UdpBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Udp
    }

    fn length(&self, _room: usize) -> Result<usize, BuildError> {
        match self.data.len().checked_add(HEADER_LEN) {
            Some(len) if len <= MAX_LEN => Ok(len),
            _ => Err(BuildError::UdpTooLong),
        }
    }

    fn write(&self, pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let length = u16::try_from(out.len()).map_err(|_| BuildError::UdpTooLong)?;
        let (header, data) = out.split_first_chunk_mut::<HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
        put_slice(data, self.data)?;
        let [a, b, c, d] = be16x2(self.source.get(), self.destination.get());
        let [l0, l1] = length.to_be_bytes();
        let unsummed = [a, b, c, d, l0, l1, 0, 0];
        let checksum = UdpChecksum::Present(pseudo.accumulator().feed(&unsummed).feed(self.data).sum().checksum());
        let [c0, c1] = checksum.field().to_be_bytes();
        put(header, [a, b, c, d, l0, l1, c0, c1]).map(|_| ())
    }
}
