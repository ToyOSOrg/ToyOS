//! ICMPv4 messages (RFC 792).
//!
//! The checksum covers the whole message and is checked before the type, so a
//! corrupted message of any type is a checksum failure. Echo and Timestamp
//! must have code 0. An error message's unassigned code is accepted as
//! unassigned rather than refused: the error still says a datagram failed.
//!
//! An error quotes the start of the datagram that caused it: a version-4
//! header of its own length and at least 8 bytes after it. The quoted total
//! length normally exceeds the quote; bytes quoted past it (RFC 4884 padding
//! and extensions) are not the original payload and are exposed apart. The
//! quoted header's checksum is not verified: the message's own checksum covers
//! the quote, and a NAT may have rewritten it.
//!
//! Types a modern host has no use for are refused by name: Source Quench
//! (RFC 6633), router discovery (RFC 1256), and the types RFC 6918 deprecates.
//!
//! ToyOS builds echoes and the two unreachable codes a host sends, protocol
//! and port, each quoting as much of the offending datagram as keeps the error
//! within 576 bytes (RFC 1812 §4.3.2.3).

use core::net::Ipv4Addr;
use core::num::NonZeroU16;

use crate::checksum::{Accumulator, Checksum, PseudoHeader, Sum};
use crate::emit::{be16x2, put, put_slice, BuildError};
use crate::ipv4::{FragmentOffset, Ipv4Packet, Ipv4Payload, Protocol};

pub const HEADER_LEN: usize = 8;
/// The most of an offending datagram an error quotes: 576 bytes less a
/// 20-byte header without options and the 8-byte ICMP header.
pub const MAX_QUOTE: usize = 548;

reasons! {
    /// Why a message was refused, in the order the checks run.
    IcmpError {
        /// Fewer than 8 bytes.
        Truncated = "icmp.truncated", Malformed;
        /// A sum over the message that is not 0xFFFF.
        Checksum = "icmp.checksum", Malformed;
        /// A nonzero code on Echo or Timestamp.
        Code = "icmp.code", Malformed;
        /// A Timestamp message other than 20 bytes.
        TimestampLength = "icmp.timestamp-length", Malformed;
        /// A quote shorter than its header and 8 bytes after it.
        QuoteTruncated = "icmp.quote-truncated", Malformed;
        /// A quote that is not an IPv4 header.
        QuoteNotIpv4 = "icmp.quote-not-ipv4", Malformed;
        /// Source Quench, deprecated by RFC 6633.
        SourceQuench = "icmp.source-quench", Unsupported;
        /// Router advertisement or solicitation (RFC 1256).
        RouterDiscovery = "icmp.router-discovery", Unsupported;
        /// A type RFC 6918 deprecates.
        DeprecatedType = "icmp.deprecated-type", Unsupported;
        /// Any other type (RFC 1122 §3.2.2).
        UnknownType = "icmp.unknown-type", Unsupported;
    }
}

/// A code the error's type assigns no meaning to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnassignedCode(u8);

impl UnassignedCode {
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Destination Unreachable's codes (RFC 792; RFC 1122 §3.2.2.1; RFC 1812 §5.2.7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreachableCode {
    Net,
    Host,
    Protocol,
    Port,
    FragmentationNeeded,
    SourceRouteFailed,
    NetUnknown,
    HostUnknown,
    SourceHostIsolated,
    NetProhibited,
    HostProhibited,
    NetUnreachableForTos,
    HostUnreachableForTos,
    CommunicationProhibited,
    HostPrecedenceViolation,
    PrecedenceCutoff,
    Unassigned(UnassignedCode),
}

impl UnreachableCode {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Net,
            1 => Self::Host,
            2 => Self::Protocol,
            3 => Self::Port,
            4 => Self::FragmentationNeeded,
            5 => Self::SourceRouteFailed,
            6 => Self::NetUnknown,
            7 => Self::HostUnknown,
            8 => Self::SourceHostIsolated,
            9 => Self::NetProhibited,
            10 => Self::HostProhibited,
            11 => Self::NetUnreachableForTos,
            12 => Self::HostUnreachableForTos,
            13 => Self::CommunicationProhibited,
            14 => Self::HostPrecedenceViolation,
            15 => Self::PrecedenceCutoff,
            code => Self::Unassigned(UnassignedCode(code)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectCode {
    Network,
    Host,
    TosNetwork,
    TosHost,
    Unassigned(UnassignedCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeExceededCode {
    /// TTL exceeded in transit.
    InTransit,
    /// Fragment reassembly time exceeded.
    Reassembly,
    Unassigned(UnassignedCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterProblemCode {
    /// The pointer indicates the error.
    Pointer,
    MissingOption,
    BadLength,
    Unassigned(UnassignedCode),
}

/// The start of the datagram an error is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote<'a> {
    header: &'a [u8; 20],
    options: &'a [u8],
    transport: &'a [u8; 8],
    payload: &'a [u8],
    beyond: &'a [u8],
}

impl<'a> Quote<'a> {
    fn parse(body: &'a [u8]) -> Result<Self, IcmpError> {
        let (header, _) = body.split_first_chunk::<20>().ok_or(IcmpError::QuoteTruncated)?;
        if header[0] >> 4 != 4 || header[0] & 0x0F < 5 {
            return Err(IcmpError::QuoteNotIpv4);
        }
        let header_len = usize::from(header[0] & 0x0F) << 2;
        let (full_header, after) = body.split_at_checked(header_len).ok_or(IcmpError::QuoteTruncated)?;
        let (transport, _) = after.split_first_chunk::<8>().ok_or(IcmpError::QuoteTruncated)?;
        let (_, options) = full_header.split_first_chunk::<20>().ok_or(IcmpError::QuoteTruncated)?;
        let total = usize::from(u16::from_be_bytes([header[2], header[3]]));
        let (original, beyond) = body.split_at_checked(total.max(header_len).min(body.len())).ok_or(IcmpError::QuoteTruncated)?;
        let (_, payload) = original.split_at_checked(header_len).ok_or(IcmpError::QuoteTruncated)?;
        Ok(Self { header, options, transport, payload, beyond })
    }

    pub const fn total_length(&self) -> u16 {
        u16::from_be_bytes([self.header[2], self.header[3]])
    }

    pub const fn identification(&self) -> u16 {
        u16::from_be_bytes([self.header[4], self.header[5]])
    }

    pub const fn fragment_offset(&self) -> FragmentOffset {
        FragmentOffset::from_field(u16::from_be_bytes([self.header[6], self.header[7]]))
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

    /// The quoted options, uninterpreted.
    pub const fn options(&self) -> &'a [u8] {
        self.options
    }

    /// The first 8 bytes after the quoted header: TCP's ports and sequence
    /// number, or UDP's whole header.
    pub const fn transport(&self) -> &'a [u8; 8] {
        self.transport
    }

    /// The quoted bytes after the header that lie within the quoted total length.
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Quoted bytes past the quoted total length: not the original datagram.
    pub const fn beyond(&self) -> &'a [u8] {
        self.beyond
    }
}

/// An echo's identifier, sequence number and data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Echo<'a> {
    pub identifier: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

/// A Timestamp or Timestamp Reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    pub identifier: u16,
    pub sequence: u16,
    pub originate: u32,
    pub receive: u32,
    pub transmit: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcmpMessage<'a> {
    EchoRequest(Echo<'a>),
    EchoReply(Echo<'a>),
    DestinationUnreachable {
        code: UnreachableCode,
        /// With code 4 only, when the router reported one (RFC 1191 §4).
        next_hop_mtu: Option<NonZeroU16>,
        quote: Quote<'a>,
    },
    Redirect { code: RedirectCode, gateway: Ipv4Addr, quote: Quote<'a> },
    TimeExceeded { code: TimeExceededCode, quote: Quote<'a> },
    ParameterProblem { code: ParameterProblemCode, pointer: u8, quote: Quote<'a> },
    TimestampRequest(Timestamp),
    TimestampReply(Timestamp),
}

/// A parsed message, borrowing the bytes it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcmpPacket<'a> {
    bytes: &'a [u8],
    checksum: Checksum,
    message: IcmpMessage<'a>,
}

impl<'a> IcmpPacket<'a> {
    /// Parses the message that is the whole of `bytes`: an IPv4 payload.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, IcmpError> {
        let (&[kind, code, c0, c1, r0, r1, r2, r3], body) = bytes.split_first_chunk::<HEADER_LEN>().ok_or(IcmpError::Truncated)?;
        if !Sum::of(bytes).verifies() {
            return Err(IcmpError::Checksum);
        }
        let echo = || match code {
            0 => Ok(Echo { identifier: u16::from_be_bytes([r0, r1]), sequence: u16::from_be_bytes([r2, r3]), data: body }),
            _ => Err(IcmpError::Code),
        };
        let unassigned = UnassignedCode(code);
        let message = match kind {
            0 => IcmpMessage::EchoReply(echo()?),
            8 => IcmpMessage::EchoRequest(echo()?),
            3 => {
                let code = UnreachableCode::from_code(code);
                let next_hop_mtu = match code {
                    UnreachableCode::FragmentationNeeded => NonZeroU16::new(u16::from_be_bytes([r2, r3])),
                    _ => None,
                };
                IcmpMessage::DestinationUnreachable { code, next_hop_mtu, quote: Quote::parse(body)? }
            }
            5 => {
                let code = match code {
                    0 => RedirectCode::Network,
                    1 => RedirectCode::Host,
                    2 => RedirectCode::TosNetwork,
                    3 => RedirectCode::TosHost,
                    _ => RedirectCode::Unassigned(unassigned),
                };
                IcmpMessage::Redirect { code, gateway: Ipv4Addr::new(r0, r1, r2, r3), quote: Quote::parse(body)? }
            }
            11 => {
                let code = match code {
                    0 => TimeExceededCode::InTransit,
                    1 => TimeExceededCode::Reassembly,
                    _ => TimeExceededCode::Unassigned(unassigned),
                };
                IcmpMessage::TimeExceeded { code, quote: Quote::parse(body)? }
            }
            12 => {
                let code = match code {
                    0 => ParameterProblemCode::Pointer,
                    1 => ParameterProblemCode::MissingOption,
                    2 => ParameterProblemCode::BadLength,
                    _ => ParameterProblemCode::Unassigned(unassigned),
                };
                IcmpMessage::ParameterProblem { code, pointer: r0, quote: Quote::parse(body)? }
            }
            13 | 14 => {
                let &[_, _, _, _, _, _, _, _, o0, o1, o2, o3, v0, v1, v2, v3, t0, t1, t2, t3] =
                    <&[u8; 20]>::try_from(bytes).map_err(|_| IcmpError::TimestampLength)?;
                if code != 0 {
                    return Err(IcmpError::Code);
                }
                let timestamp = Timestamp {
                    identifier: u16::from_be_bytes([r0, r1]),
                    sequence: u16::from_be_bytes([r2, r3]),
                    originate: u32::from_be_bytes([o0, o1, o2, o3]),
                    receive: u32::from_be_bytes([v0, v1, v2, v3]),
                    transmit: u32::from_be_bytes([t0, t1, t2, t3]),
                };
                if kind == 13 {
                    IcmpMessage::TimestampRequest(timestamp)
                } else {
                    IcmpMessage::TimestampReply(timestamp)
                }
            }
            4 => return Err(IcmpError::SourceQuench),
            9 | 10 => return Err(IcmpError::RouterDiscovery),
            6 | 15..=18 | 30..=39 => return Err(IcmpError::DeprecatedType),
            _ => return Err(IcmpError::UnknownType),
        };
        Ok(Self { bytes, checksum: Checksum::from_field(u16::from_be_bytes([c0, c1])), message })
    }

    pub const fn message(&self) -> IcmpMessage<'a> {
        self.message
    }

    pub const fn checksum(&self) -> Checksum {
        self.checksum
    }

    /// The message as received.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Writes a message: `kind`, `code`, the checksum, four type-specific bytes
/// and `body`, the checksum over all of it.
fn write_message(out: &mut [u8], kind: u8, code: u8, word: [u8; 4], body: &[u8]) -> Result<(), BuildError> {
    let (header, rest) = out.split_first_chunk_mut::<HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
    put_slice(rest, body)?;
    let [w0, w1, w2, w3] = word;
    let unsummed = [kind, code, 0, 0, w0, w1, w2, w3];
    let [c0, c1] = Accumulator::new().feed(&unsummed).feed(body).sum().checksum().to_be_bytes();
    put(header, [kind, code, c0, c1, w0, w1, w2, w3]).map(|_| ())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoKind {
    Request,
    Reply,
}

/// An echo to build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoBuilder<'a> {
    pub kind: EchoKind,
    pub identifier: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

impl<'a> EchoBuilder<'a> {
    /// The reply to `request`: its identifier, sequence number and every byte
    /// of its data (RFC 1122 §3.2.2.6).
    pub const fn reply_to(request: &Echo<'a>) -> Self {
        Self { kind: EchoKind::Reply, identifier: request.identifier, sequence: request.sequence, data: request.data }
    }
}

impl Ipv4Payload for EchoBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Icmp
    }

    fn length(&self, _room: usize) -> Result<usize, BuildError> {
        Ok(HEADER_LEN.saturating_add(self.data.len()))
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let kind = match self.kind {
            EchoKind::Request => 8,
            EchoKind::Reply => 0,
        };
        write_message(out, kind, 0, be16x2(self.identifier, self.sequence), self.data)
    }
}

/// The unreachable codes a host sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostUnreachable {
    Protocol,
    Port,
}

/// A Destination Unreachable about `datagram`, quoting it as received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreachableBuilder<'a> {
    pub code: HostUnreachable,
    pub datagram: &'a Ipv4Packet<'a>,
}

impl UnreachableBuilder<'_> {
    fn quote(&self) -> &[u8] {
        let bytes = self.datagram.bytes();
        bytes.get(..MAX_QUOTE).unwrap_or(bytes)
    }
}

impl Ipv4Payload for UnreachableBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Icmp
    }

    fn length(&self, _room: usize) -> Result<usize, BuildError> {
        Ok(HEADER_LEN.saturating_add(self.quote().len()))
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let code = match self.code {
            HostUnreachable::Protocol => 2,
            HostUnreachable::Port => 3,
        };
        write_message(out, 3, code, [0; 4], self.quote())
    }
}
