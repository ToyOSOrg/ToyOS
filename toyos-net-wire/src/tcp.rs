//! TCP segments and their options (RFC 9293 §3.1).
//!
//! The checksum covers the pseudo-header with the IPv4 payload length as the
//! TCP length, and there is no "no checksum" value. No flag combination is
//! refused: SYN with FIN, or no flags at all, parse faithfully and the state
//! machine decides what they mean. The window is exposed as sent, never
//! scaled. The reserved bits are ignored and kept in the bytes.
//!
//! Every option in every segment is walked and validated (MUST-5); an option
//! of illegal length refuses the whole segment, so a forged option drops a
//! segment rather than resetting a connection (MUST-7 suggests the reset).
//! Unknown kinds are skipped by their length (MUST-6). A repeated option keeps
//! its last value.
//!
//! A built segment's options are typed by where they may go: a SYN carries MSS,
//! Window Scale, SACK-Permitted and Timestamps, and an established segment has
//! no field for any of the first three. They are laid out as deployed stacks
//! send them, each unit on a 4-byte boundary. URG is never sent.

use crate::checksum::{Checksum, PseudoHeader};
use crate::emit::{be16x2, put, put_slice, BuildError};
use crate::ipv4::{Ipv4Packet, Ipv4Payload, Protocol};
use crate::Port;

pub const MIN_HEADER_LEN: usize = 20;
/// The largest Window Scale shift that means anything (RFC 7323 §2.3).
pub const MAX_WINDOW_SHIFT: u8 = 14;

reasons! {
    /// Why a segment was refused, in the order the checks run.
    TcpError {
        /// Fewer than 20 bytes.
        Truncated = "tcp.truncated", Malformed;
        /// A data offset below 5 words.
        DataOffset = "tcp.data-offset", Malformed;
        /// Fewer bytes than the data offset names.
        HeaderOverrun = "tcp.header-overrun", Malformed;
        /// A sum over pseudo-header and segment that is not 0xFFFF.
        Checksum = "tcp.checksum", Malformed;
        /// Source or destination port 0.
        PortZero = "tcp.port-zero", Malformed;
        /// An option length below 2, or wrong for its kind.
        OptionLength = "tcp.option-length", Malformed;
        /// An option that runs past the options area.
        OptionOverrun = "tcp.option-overrun", Malformed;
        /// A SACK option whose blocks are not whole (RFC 2018 §3).
        SackLength = "tcp.sack-length", Malformed;
    }
}

/// A sequence or acknowledgment number. It wraps, so it has no order here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SeqNum(u32);

impl SeqNum {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// The window field as sent: scaling is connection state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawWindow(pub u16);

/// The eight flag bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpFlags(u8);

impl TcpFlags {
    pub const NONE: Self = Self(0);
    pub const FIN: Self = Self(0x01);
    pub const SYN: Self = Self(0x02);
    pub const RST: Self = Self(0x04);
    pub const PSH: Self = Self(0x08);
    pub const ACK: Self = Self(0x10);
    pub const URG: Self = Self(0x20);
    pub const ECE: Self = Self(0x40);
    pub const CWR: Self = Self(0x80);

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, flags: Self) -> bool {
        self.0 & flags.0 == flags.0
    }
}

impl core::ops::BitOr for TcpFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The Timestamps option's two values (RFC 7323 §3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamps {
    /// TSval.
    pub value: u32,
    /// TSecr.
    pub echo: u32,
}

impl Timestamps {
    /// The option at a 4-byte boundary, after the two bytes `lead`.
    fn unit(self, lead: [u8; 2]) -> [u8; 12] {
        let [v0, v1, v2, v3] = self.value.to_be_bytes();
        let [e0, e1, e2, e3] = self.echo.to_be_bytes();
        let [l0, l1] = lead;
        [l0, l1, 8, 10, v0, v1, v2, v3, e0, e1, e2, e3]
    }
}

/// One SACK block: raw edges, whose meaning is the connection's question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SackBlock {
    pub left: SeqNum,
    pub right: SeqNum,
}

/// A received Window Scale shift, as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowScale(u8);

impl WindowScale {
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// The shift used: a larger one is used as 14 (RFC 7323 §2.3).
    pub const fn effective(self) -> u8 {
        if self.0 > MAX_WINDOW_SHIFT {
            MAX_WINDOW_SHIFT
        } else {
            self.0
        }
    }
}

/// A Window Scale shift a SYN may be built with: at most 14.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowShift(u8);

impl WindowShift {
    pub const fn new(shift: u8) -> Result<Self, BuildError> {
        if shift > MAX_WINDOW_SHIFT {
            Err(BuildError::TcpWindowScaleTooLarge)
        } else {
            Ok(Self(shift))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

/// What a segment's options said; a repeated option kept its last value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpOptions<'a> {
    mss: Option<u16>,
    window_scale: Option<WindowScale>,
    sack_permitted: bool,
    timestamps: Option<Timestamps>,
    sack: &'a [[u8; 8]],
}

/// `data` as exactly `N` bytes, or the option's length was wrong.
fn exactly<const N: usize>(data: &[u8]) -> Result<&[u8; N], TcpError> {
    <&[u8; N]>::try_from(data).map_err(|_| TcpError::OptionLength)
}

impl<'a> TcpOptions<'a> {
    fn parse(mut area: &'a [u8]) -> Result<Self, TcpError> {
        let mut options = Self::default();
        while let Some((&kind, rest)) = area.split_first() {
            match kind {
                0 => break,
                1 => {
                    area = rest;
                    continue;
                }
                _ => {}
            }
            let &length = rest.first().ok_or(TcpError::OptionOverrun)?;
            if length < 2 {
                return Err(TcpError::OptionLength);
            }
            let (option, after) = area.split_at_checked(usize::from(length)).ok_or(TcpError::OptionOverrun)?;
            let (_, data) = option.split_first_chunk::<2>().ok_or(TcpError::OptionLength)?;
            match kind {
                2 => options.mss = Some(u16::from_be_bytes(*exactly::<2>(data)?)),
                3 => {
                    let &[shift] = exactly::<1>(data)?;
                    options.window_scale = Some(WindowScale(shift));
                }
                4 => {
                    exactly::<0>(data)?;
                    options.sack_permitted = true;
                }
                5 => {
                    let (blocks, partial) = data.as_chunks::<8>();
                    if !partial.is_empty() {
                        return Err(TcpError::SackLength);
                    }
                    options.sack = blocks;
                }
                8 => {
                    let &[v0, v1, v2, v3, e0, e1, e2, e3] = exactly::<8>(data)?;
                    options.timestamps = Some(Timestamps {
                        value: u32::from_be_bytes([v0, v1, v2, v3]),
                        echo: u32::from_be_bytes([e0, e1, e2, e3]),
                    });
                }
                _ => {}
            }
            area = after;
        }
        Ok(options)
    }

    /// The MSS as received, 0 included.
    pub const fn mss(&self) -> Option<u16> {
        self.mss
    }

    pub const fn window_scale(&self) -> Option<WindowScale> {
        self.window_scale
    }

    pub const fn sack_permitted(&self) -> bool {
        self.sack_permitted
    }

    pub const fn timestamps(&self) -> Option<Timestamps> {
        self.timestamps
    }

    /// The last SACK option's blocks; none when it had none or there was none.
    pub fn sack_blocks(&self) -> impl ExactSizeIterator<Item = SackBlock> + 'a {
        self.sack.iter().map(|&[l0, l1, l2, l3, r0, r1, r2, r3]| SackBlock {
            left: SeqNum(u32::from_be_bytes([l0, l1, l2, l3])),
            right: SeqNum(u32::from_be_bytes([r0, r1, r2, r3])),
        })
    }
}

/// A parsed segment, borrowing the bytes it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpSegment<'a> {
    header: &'a [u8; MIN_HEADER_LEN],
    source: Port,
    destination: Port,
    options_area: &'a [u8],
    options: TcpOptions<'a>,
    payload: &'a [u8],
}

impl<'a> TcpSegment<'a> {
    /// Parses the segment `ip` carries, verified against `ip`'s addresses.
    pub fn parse(ip: &Ipv4Packet<'a>) -> Result<Self, TcpError> {
        let bytes = ip.payload();
        let (header, _) = bytes.split_first_chunk::<MIN_HEADER_LEN>().ok_or(TcpError::Truncated)?;
        let header_len = usize::from(header[12] >> 4) << 2;
        if header_len < MIN_HEADER_LEN {
            return Err(TcpError::DataOffset);
        }
        let (full_header, payload) = bytes.split_at_checked(header_len).ok_or(TcpError::HeaderOverrun)?;
        let length = u16::try_from(bytes.len()).map_err(|_| TcpError::Checksum)?;
        if !ip.pseudo_header(length).accumulator().feed(bytes).sum().verifies() {
            return Err(TcpError::Checksum);
        }
        let (source, destination) = Port::new(u16::from_be_bytes([header[0], header[1]]))
            .zip(Port::new(u16::from_be_bytes([header[2], header[3]])))
            .ok_or(TcpError::PortZero)?;
        let (_, options_area) = full_header.split_first_chunk::<MIN_HEADER_LEN>().ok_or(TcpError::Truncated)?;
        let options = TcpOptions::parse(options_area)?;
        Ok(Self { header, source, destination, options_area, options, payload })
    }

    pub const fn source_port(&self) -> Port {
        self.source
    }

    pub const fn destination_port(&self) -> Port {
        self.destination
    }

    pub const fn sequence(&self) -> SeqNum {
        SeqNum(u32::from_be_bytes([self.header[4], self.header[5], self.header[6], self.header[7]]))
    }

    /// Present only when ACK is set.
    pub const fn acknowledgment(&self) -> Option<SeqNum> {
        if self.flags().contains(TcpFlags::ACK) {
            Some(SeqNum(u32::from_be_bytes([self.header[8], self.header[9], self.header[10], self.header[11]])))
        } else {
            None
        }
    }

    /// The header length the data offset names.
    pub const fn header_len(&self) -> usize {
        MIN_HEADER_LEN.saturating_add(self.options_area.len())
    }

    /// All eight flags as sent.
    pub const fn flags(&self) -> TcpFlags {
        TcpFlags(self.header[13])
    }

    pub const fn window(&self) -> RawWindow {
        RawWindow(u16::from_be_bytes([self.header[14], self.header[15]]))
    }

    pub const fn checksum(&self) -> Checksum {
        Checksum::from_field(u16::from_be_bytes([self.header[16], self.header[17]]))
    }

    /// Present only when URG is set.
    pub const fn urgent_pointer(&self) -> Option<u16> {
        if self.flags().contains(TcpFlags::URG) {
            Some(u16::from_be_bytes([self.header[18], self.header[19]]))
        } else {
            None
        }
    }

    pub const fn options(&self) -> TcpOptions<'a> {
        self.options
    }

    /// The fixed header as received.
    pub const fn header(&self) -> &'a [u8; MIN_HEADER_LEN] {
        self.header
    }

    /// The options area as received, layout and padding included.
    pub const fn options_bytes(&self) -> &'a [u8] {
        self.options_area
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// The options a SYN or SYN-ACK may carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SynOptions {
    pub mss: Option<u16>,
    pub sack_permitted: bool,
    pub timestamps: Option<Timestamps>,
    pub window_scale: Option<WindowShift>,
}

impl SynOptions {
    fn len(&self) -> usize {
        let mss: usize = if self.mss.is_some() { 4 } else { 0 };
        let sack_timestamps = match (self.sack_permitted, self.timestamps) {
            (_, Some(_)) => 12,
            (true, None) => 4,
            (false, None) => 0,
        };
        let shift = if self.window_scale.is_some() { 4 } else { 0 };
        mss.saturating_add(sack_timestamps).saturating_add(shift)
    }

    fn write(&self, mut out: &mut [u8]) -> Result<(), BuildError> {
        if let Some(mss) = self.mss {
            let [m0, m1] = mss.to_be_bytes();
            out = put(out, [2, 4, m0, m1])?;
        }
        out = match (self.sack_permitted, self.timestamps) {
            (true, Some(timestamps)) => put(out, timestamps.unit([4, 2]))?,
            (false, Some(timestamps)) => put(out, timestamps.unit([1, 1]))?,
            (true, None) => put(out, [1, 1, 4, 2])?,
            (false, None) => out,
        };
        if let Some(shift) = self.window_scale {
            put(out, [1, 3, 3, shift.0])?;
        }
        Ok(())
    }
}

/// The options any segment after the handshake may carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EstablishedOptions<'a> {
    pub timestamps: Option<Timestamps>,
    pub sack: &'a [SackBlock],
}

impl EstablishedOptions<'_> {
    fn len(&self) -> Result<usize, BuildError> {
        let (timestamps, max_blocks) = if self.timestamps.is_some() { (12, 3) } else { (0, 4) };
        match self.sack.len() {
            0 => Ok(timestamps),
            n if n <= max_blocks => Ok(n.saturating_mul(8).saturating_add(4).saturating_add(timestamps)),
            _ => Err(BuildError::TcpTooManySackBlocks),
        }
    }

    fn write(&self, mut out: &mut [u8]) -> Result<(), BuildError> {
        if let Some(timestamps) = self.timestamps {
            out = put(out, timestamps.unit([1, 1]))?;
        }
        if let Some(len) = u8::try_from(self.sack.len()).ok().filter(|&n| n > 0) {
            out = put(out, [1, 1, 5, len.saturating_mul(8).saturating_add(2)])?;
            for block in self.sack {
                let [a, b, c, d] = block.left.0.to_be_bytes();
                let [e, f, g, h] = block.right.0.to_be_bytes();
                out = put(out, [a, b, c, d, e, f, g, h])?;
            }
        }
        Ok(())
    }
}

/// What a segment does, with the options that may go with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control<'a> {
    Syn(SynOptions),
    SynAck { acknowledgment: SeqNum, options: SynOptions },
    /// ACK set, with PSH and FIN as asked.
    Ack { acknowledgment: SeqNum, push: bool, fin: bool, options: EstablishedOptions<'a> },
    /// RST, with ACK when an acknowledgment is given.
    Rst { acknowledgment: Option<SeqNum>, options: EstablishedOptions<'a> },
}

/// A segment to build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpBuilder<'a> {
    pub source: Port,
    pub destination: Port,
    pub sequence: SeqNum,
    pub control: Control<'a>,
    pub window: RawWindow,
    pub data: &'a [u8],
}

impl TcpBuilder<'_> {
    fn options_len(&self) -> Result<usize, BuildError> {
        match &self.control {
            Control::Syn(options) | Control::SynAck { options, .. } => Ok(options.len()),
            Control::Ack { options, .. } | Control::Rst { options, .. } => options.len(),
        }
    }
}

impl Ipv4Payload for TcpBuilder<'_> {
    fn protocol(&self) -> Protocol {
        Protocol::Tcp
    }

    fn length(&self, room: usize) -> Result<usize, BuildError> {
        match MIN_HEADER_LEN.checked_add(self.options_len()?).and_then(|header| header.checked_add(self.data.len())) {
            Some(len) if len <= room => Ok(len),
            _ => Err(BuildError::TcpTooLong),
        }
    }

    fn write(&self, pseudo: &PseudoHeader, out: &mut [u8]) -> Result<(), BuildError> {
        let options_len = self.options_len()?;
        let (header, rest) = out.split_first_chunk_mut::<MIN_HEADER_LEN>().ok_or(BuildError::BufferTooSmall)?;
        let (options, data) = rest.split_at_mut_checked(options_len).ok_or(BuildError::BufferTooSmall)?;
        let (flags, acknowledgment) = match &self.control {
            Control::Syn(syn) => {
                syn.write(options)?;
                (TcpFlags::SYN, None)
            }
            Control::SynAck { acknowledgment, options: syn } => {
                syn.write(options)?;
                (TcpFlags::SYN | TcpFlags::ACK, Some(*acknowledgment))
            }
            Control::Ack { acknowledgment, push, fin, options: established } => {
                established.write(options)?;
                let push = if *push { TcpFlags::PSH } else { TcpFlags::NONE };
                let fin = if *fin { TcpFlags::FIN } else { TcpFlags::NONE };
                (TcpFlags::ACK | push | fin, Some(*acknowledgment))
            }
            Control::Rst { acknowledgment, options: established } => {
                established.write(options)?;
                let ack = if acknowledgment.is_some() { TcpFlags::ACK } else { TcpFlags::NONE };
                (TcpFlags::RST | ack, *acknowledgment)
            }
        };
        put_slice(data, self.data)?;
        let words = u8::try_from(MIN_HEADER_LEN.saturating_add(options_len) >> 2).map_err(|_| BuildError::TcpTooLong)?;
        let [p0, p1, p2, p3] = be16x2(self.source.get(), self.destination.get());
        let [s0, s1, s2, s3] = self.sequence.0.to_be_bytes();
        let [a0, a1, a2, a3] = acknowledgment.map_or(0, SeqNum::get).to_be_bytes();
        let [w0, w1] = self.window.0.to_be_bytes();
        let mut fixed = [p0, p1, p2, p3, s0, s1, s2, s3, a0, a1, a2, a3, words << 4, flags.0, w0, w1, 0, 0, 0, 0];
        let [c0, c1] = pseudo.accumulator().feed(&fixed).feed(options).feed(self.data).sum().checksum().to_be_bytes();
        fixed[16] = c0;
        fixed[17] = c1;
        *header = fixed;
        Ok(())
    }
}
