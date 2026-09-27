//! TCP segments and their options (RFC 9293 §3.1).

use crate::checksum::PseudoHeader;
use crate::emit::{be16x2, put, put_slice, BuildError};
use crate::ipv4::{Ipv4Packet, Ipv4Payload, Protocol};
use crate::Port;

pub const MIN_HEADER_LEN: usize = 20;
pub const MAX_WINDOW_SHIFT: u8 = 14;

reasons! {
    TcpError {
        Truncated = "tcp.truncated", Malformed;
        DataOffset = "tcp.data-offset", Malformed;
        HeaderOverrun = "tcp.header-overrun", Malformed;
        Checksum = "tcp.checksum", Malformed;
        PortZero = "tcp.port-zero", Malformed;
        OptionLength = "tcp.option-length", Malformed;
        OptionOverrun = "tcp.option-overrun", Malformed;
        SackLength = "tcp.sack-length", Malformed;
    }
}

/// It wraps, so it has no order here.
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

/// As sent: scaling is connection state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawWindow(pub u16);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamps {
    pub value: u32,
    pub echo: u32,
}

impl Timestamps {
    fn unit(self, lead: [u8; 2]) -> [u8; 12] {
        let [v0, v1, v2, v3] = self.value.to_be_bytes();
        let [e0, e1, e2, e3] = self.echo.to_be_bytes();
        let [l0, l1] = lead;
        [l0, l1, 8, 10, v0, v1, v2, v3, e0, e1, e2, e3]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SackBlock {
    pub left: SeqNum,
    pub right: SeqNum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowScale(u8);

impl WindowScale {
    pub const fn raw(self) -> u8 {
        self.0
    }

    pub const fn effective(self) -> u8 {
        if self.0 > MAX_WINDOW_SHIFT {
            MAX_WINDOW_SHIFT
        } else {
            self.0
        }
    }
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TcpOptions<'a> {
    mss: Option<u16>,
    window_scale: Option<WindowScale>,
    sack_permitted: bool,
    timestamps: Option<Timestamps>,
    sack: &'a [[u8; 8]],
}

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
            // An illegal length drops the segment: MUST-7's reset would let a forged option end a connection.
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

    pub fn sack_blocks(&self) -> impl ExactSizeIterator<Item = SackBlock> + 'a {
        self.sack.iter().map(|&[l0, l1, l2, l3, r0, r1, r2, r3]| SackBlock {
            left: SeqNum(u32::from_be_bytes([l0, l1, l2, l3])),
            right: SeqNum(u32::from_be_bytes([r0, r1, r2, r3])),
        })
    }
}

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

    pub const fn acknowledgment(&self) -> Option<SeqNum> {
        if self.flags().contains(TcpFlags::ACK) {
            Some(SeqNum(u32::from_be_bytes([self.header[8], self.header[9], self.header[10], self.header[11]])))
        } else {
            None
        }
    }

    pub const fn header_len(&self) -> usize {
        MIN_HEADER_LEN.saturating_add(self.options_area.len())
    }

    pub const fn flags(&self) -> TcpFlags {
        TcpFlags(self.header[13])
    }

    pub const fn window(&self) -> RawWindow {
        RawWindow(u16::from_be_bytes([self.header[14], self.header[15]]))
    }

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

    pub const fn header(&self) -> &'a [u8; MIN_HEADER_LEN] {
        self.header
    }

    pub const fn options_bytes(&self) -> &'a [u8] {
        self.options_area
    }

    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control<'a> {
    Syn(SynOptions),
    SynAck { acknowledgment: SeqNum, options: SynOptions },
    Ack { acknowledgment: SeqNum, push: bool, fin: bool, options: EstablishedOptions<'a> },
    Rst { acknowledgment: Option<SeqNum>, options: EstablishedOptions<'a> },
}

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
