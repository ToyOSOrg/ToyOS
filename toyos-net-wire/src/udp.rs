//! UDP datagrams (RFC 768).

use crate::checksum::{Checksum, PseudoHeader};
use crate::emit::{be16x2, BuildError};
use crate::ipv4::{Ipv4Packet, Protocol, WritePayload};
use crate::Port;

pub const HEADER_LEN: usize = 8;
pub const MAX_LEN: usize = 65535;

reasons! {
    UdpError {
        Truncated = "udp.truncated", Malformed;
        LengthBelowHeader = "udp.length-below-header", Malformed;
        LengthOverrun = "udp.length-overrun", Malformed;
        Checksum = "udp.checksum", Malformed;
        DestinationPortZero = "udp.destination-port-zero", Malformed;
    }
}

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

    /// A computed 0x0000 is sent as 0xFFFF (RFC 768).
    pub const fn field(self) -> u16 {
        match self {
            Self::Absent => 0,
            Self::Present(checksum) => match checksum.value() {
                0 => 0xFFFF,
                value => value,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpDatagram<'a> {
    header: &'a [u8; HEADER_LEN],
    source: Option<Port>,
    destination: Port,
    payload: &'a [u8],
}

impl<'a> UdpDatagram<'a> {
    pub fn parse(ip: &Ipv4Packet<'a>) -> Result<Self, UdpError> {
        let bytes = ip.payload();
        let (&[s0, s1, d0, d1, l0, l1, c0, c1], _) = bytes.split_first_chunk::<HEADER_LEN>().ok_or(UdpError::Truncated)?;
        let length = u16::from_be_bytes([l0, l1]);
        if usize::from(length) < HEADER_LEN {
            return Err(UdpError::LengthBelowHeader);
        }
        let (datagram, _) = bytes.split_at_checked(usize::from(length)).ok_or(UdpError::LengthOverrun)?;
        let pseudo = PseudoHeader { source: ip.source(), destination: ip.destination(), protocol: Protocol::Udp, length };
        if UdpChecksum::from_field(u16::from_be_bytes([c0, c1])) != UdpChecksum::Absent
            && !pseudo.accumulator().feed(datagram).sum().verifies()
        {
            return Err(UdpError::Checksum);
        }
        let destination = Port::new(u16::from_be_bytes([d0, d1])).ok_or(UdpError::DestinationPortZero)?;
        let (header, payload) = datagram.split_first_chunk::<HEADER_LEN>().ok_or(UdpError::Truncated)?;
        Ok(Self { header, source: Port::new(u16::from_be_bytes([s0, s1])), destination, payload })
    }

    pub const fn source_port(&self) -> Option<Port> {
        self.source
    }

    pub const fn destination_port(&self) -> Port {
        self.destination
    }

    pub const fn checksum(&self) -> UdpChecksum {
        UdpChecksum::from_field(u16::from_be_bytes([self.header[6], self.header[7]]))
    }

    pub const fn header(&self) -> &'a [u8; HEADER_LEN] {
        self.header
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpBuilder<'a> {
    pub source: Port,
    pub destination: Port,
    pub data: &'a [u8],
}

impl WritePayload for UdpBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Udp
    }

    fn length(&self, _header_len: usize) -> Result<usize, BuildError> {
        match self.data.len().checked_add(HEADER_LEN) {
            Some(len) if len <= MAX_LEN => Ok(len),
            _ => Err(BuildError::UdpTooLong),
        }
    }

    fn write(&self, pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let length = u16::try_from(out.len()).map_err(|_| BuildError::UdpTooLong)?;
        let (header, data) = out.split_first_chunk_mut::<HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
        let [a, b, c, d] = be16x2(self.source.get(), self.destination.get());
        let [l0, l1] = length.to_be_bytes();
        let sum = pseudo.accumulator().feed(&[a, b, c, d, l0, l1]).copy(data, self.data)?.sum();
        let [c0, c1] = UdpChecksum::Present(sum.checksum()).field().to_be_bytes();
        *header = [a, b, c, d, l0, l1, c0, c1];
        Ok(())
    }
}
