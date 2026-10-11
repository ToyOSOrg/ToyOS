//! ICMPv4 messages (RFC 792).

use core::net::Ipv4Addr;
use core::num::NonZeroU16;

use crate::checksum::{Accumulator, PseudoHeader, Sum};
use crate::emit::{be16x2, BuildError};
use crate::ipv4::{FragmentOffset, Ipv4Packet, Protocol, WritePayload};

pub const HEADER_LEN: usize = 8;
/// RFC 1812 §4.3.2.3: an error ToyOS sends, IPv4 header included, quotes no more than fits.
pub const MAX_ERROR_LEN: usize = 576;

reasons! {
    IcmpError {
        Truncated = "icmp.truncated", Malformed;
        Checksum = "icmp.checksum", Malformed;
        Code = "icmp.code", Malformed;
        QuoteTruncated = "icmp.quote-truncated", Malformed;
        QuoteNotIpv4 = "icmp.quote-not-ipv4", Malformed;
        QuoteTotalLengthBelowHeader = "icmp.quote-total-length-below-header", Malformed;
        TimestampRequest = "icmp.timestamp-request", Unsupported;
        TimestampReply = "icmp.timestamp-reply", Unsupported;
        SourceQuench = "icmp.source-quench", Unsupported;
        RouterDiscovery = "icmp.router-discovery", Unsupported;
        DeprecatedType = "icmp.deprecated-type", Unsupported;
        UnknownType = "icmp.unknown-type", Unsupported;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnassignedCode(u8);

impl UnassignedCode {
    pub const fn value(self) -> u8 {
        self.0
    }
}

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
    InTransit,
    Reassembly,
    Unassigned(UnassignedCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterProblemCode {
    Pointer,
    MissingOption,
    BadLength,
    Unassigned(UnassignedCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quote<'a> {
    header: &'a [u8; 20],
    options: &'a [u8],
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
        let (_, options) = full_header.split_first_chunk::<20>().ok_or(IcmpError::QuoteTruncated)?;
        let within = usize::from(u16::from_be_bytes([header[2], header[3]]))
            .checked_sub(header_len)
            .ok_or(IcmpError::QuoteTotalLengthBelowHeader)?;
        // The first 8 payload bytes are owed, or every one when the datagram has fewer.
        if after.len() < within.min(8) {
            return Err(IcmpError::QuoteTruncated);
        }
        let (payload, beyond) = after.split_at(within.min(after.len()));
        Ok(Self { header, options, payload, beyond })
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

    pub const fn options(&self) -> &'a [u8] {
        self.options
    }

    pub const fn transport(&self) -> Option<&'a [u8; 8]> {
        self.payload.first_chunk::<8>()
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    pub const fn beyond(&self) -> &'a [u8] {
        self.beyond
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Echo<'a> {
    pub identifier: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcmpMessage<'a> {
    EchoRequest(Echo<'a>),
    EchoReply(Echo<'a>),
    DestinationUnreachable {
        code: UnreachableCode,
        next_hop_mtu: Option<NonZeroU16>,
        quote: Quote<'a>,
    },
    Redirect { code: RedirectCode, gateway: Ipv4Addr, quote: Quote<'a> },
    TimeExceeded { code: TimeExceededCode, quote: Quote<'a> },
    ParameterProblem { code: ParameterProblemCode, pointer: u8, quote: Quote<'a> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcmpPacket<'a> {
    bytes: &'a [u8],
    message: IcmpMessage<'a>,
}

impl<'a> IcmpPacket<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, IcmpError> {
        let (&[kind, code, _, _, r0, r1, r2, r3], body) = bytes.split_first_chunk::<HEADER_LEN>().ok_or(IcmpError::Truncated)?;
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
            13 => return Err(IcmpError::TimestampRequest),
            14 => return Err(IcmpError::TimestampReply),
            4 => return Err(IcmpError::SourceQuench),
            9 | 10 => return Err(IcmpError::RouterDiscovery),
            6 | 15..=18 | 30..=39 => return Err(IcmpError::DeprecatedType),
            _ => return Err(IcmpError::UnknownType),
        };
        Ok(Self { bytes, message })
    }

    pub const fn message(&self) -> IcmpMessage<'a> {
        self.message
    }

    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

fn write_message(out: &mut [u8], kind: u8, code: u8, word: [u8; 4], body: &[u8]) -> Result<(), BuildError> {
    let (header, rest) = out.split_first_chunk_mut::<HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
    let [w0, w1, w2, w3] = word;
    let [c0, c1] = Accumulator::new().feed(&[kind, code, 0, 0, w0, w1, w2, w3]).copy(rest, body)?.sum().checksum().to_be_bytes();
    *header = [kind, code, c0, c1, w0, w1, w2, w3];
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoKind {
    Request,
    Reply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoBuilder<'a> {
    pub kind: EchoKind,
    pub identifier: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

impl<'a> EchoBuilder<'a> {
    pub const fn reply_to(request: &Echo<'a>) -> Self {
        Self { kind: EchoKind::Reply, identifier: request.identifier, sequence: request.sequence, data: request.data }
    }
}

impl WritePayload for EchoBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Icmp
    }

    fn length(&self, _header_len: usize) -> Result<usize, BuildError> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostUnreachable {
    Protocol,
    Port,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreachableBuilder<'a> {
    pub code: HostUnreachable,
    pub datagram: &'a Ipv4Packet<'a>,
}

impl WritePayload for UnreachableBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Icmp
    }

    fn length(&self, header_len: usize) -> Result<usize, BuildError> {
        let room = MAX_ERROR_LEN.saturating_sub(header_len).saturating_sub(HEADER_LEN);
        Ok(HEADER_LEN.saturating_add(self.datagram.bytes().len().min(room)))
    }

    fn write(&self, _pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let code = match self.code {
            HostUnreachable::Protocol => 2,
            HostUnreachable::Port => 3,
        };
        let quote = self.datagram.bytes().get(..out.len().saturating_sub(HEADER_LEN)).ok_or(BuildError::BufferTooSmall)?;
        write_message(out, 3, code, [0; 4], quote)
    }
}
