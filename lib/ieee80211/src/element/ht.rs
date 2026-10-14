//! HT Capabilities (§9.4.2.55), HT Operation (§9.4.2.56), VHT Capabilities (§9.4.2.157) and VHT
//! Operation (§9.4.2.158): fixed-length elements, refused at any other length or where a field
//! holds a value the standard reserves.

use crate::read::Reader;
use crate::Refused;

fn exact<const N: usize>(body: &[u8]) -> Result<[u8; N], Refused> {
    body.try_into().map_err(|_| Refused::ElementLength)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtCapabilities {
    pub info: u16,
    pub ampdu_parameters: u8,
    /// The Supported MCS Set field; its first 77 bits are the Rx MCS Bitmask.
    pub mcs: [u8; 16],
    pub extended: u16,
    pub beamforming: u32,
    pub antenna_selection: u8,
}

impl HtCapabilities {
    pub(super) fn parse(body: &[u8]) -> Result<Self, Refused> {
        let bytes: [u8; 26] = exact(body)?;
        let mut r = Reader::new(&bytes);
        Ok(Self {
            info: r.u16()?,
            ampdu_parameters: r.u8()?,
            mcs: r.array()?,
            extended: r.u16()?,
            beamforming: r.u32()?,
            antenna_selection: r.u8()?,
        })
    }
}

/// Secondary Channel Offset (§9.4.2.56).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondaryChannel {
    None,
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtOperation {
    pub primary_channel: u8,
    pub secondary: SecondaryChannel,
    /// STA Channel Width: any channel width in the HT Capabilities' set.
    pub any_width: bool,
    pub info: [u8; 5],
    pub basic_mcs: [u8; 16],
}

impl HtOperation {
    pub(super) fn parse(body: &[u8]) -> Result<Self, Refused> {
        let bytes: [u8; 22] = exact(body)?;
        let mut r = Reader::new(&bytes);
        let primary_channel = r.u8()?;
        let info: [u8; 5] = r.array()?;
        let [first, ..] = info;
        let secondary = match first & 0b11 {
            0 => SecondaryChannel::None,
            1 => SecondaryChannel::Above,
            3 => SecondaryChannel::Below,
            _ => return Err(Refused::SecondaryChannelOffset),
        };
        Ok(Self { primary_channel, secondary, any_width: first & 1 << 2 != 0, info, basic_mcs: r.array()? })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhtCapabilities {
    pub info: u32,
    pub rx_mcs_map: u16,
    pub rx_highest: u16,
    pub tx_mcs_map: u16,
    pub tx_highest: u16,
}

impl VhtCapabilities {
    pub(super) fn parse(body: &[u8]) -> Result<Self, Refused> {
        let bytes: [u8; 12] = exact(body)?;
        let mut r = Reader::new(&bytes);
        let caps = Self {
            info: r.u32()?,
            rx_mcs_map: r.u16()?,
            rx_highest: r.u16()?,
            tx_mcs_map: r.u16()?,
            tx_highest: r.u16()?,
        };
        if caps.info & 0b11 == 0b11 || (caps.info >> 2) & 0b11 == 0b11 {
            return Err(Refused::VhtReserved);
        }
        Ok(caps)
    }
}

/// The VHT Operation Information's Channel Width (Table 9-274).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VhtChannelWidth {
    TwentyOrForty,
    /// 80, 160 or 80+80 MHz, told apart by the center frequency segments.
    Eighty,
    /// 160 MHz, in the deprecated encoding.
    OneSixty,
    /// Non-contiguous 80+80 MHz, in the deprecated encoding.
    EightyPlusEighty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhtOperation {
    pub width: VhtChannelWidth,
    pub center_segment0: u8,
    pub center_segment1: u8,
    pub basic_mcs_map: u16,
}

impl VhtOperation {
    pub(super) fn parse(body: &[u8]) -> Result<Self, Refused> {
        let [width, center_segment0, center_segment1, m0, m1] = exact(body)?;
        let width = match width {
            0 => VhtChannelWidth::TwentyOrForty,
            1 => VhtChannelWidth::Eighty,
            2 => VhtChannelWidth::OneSixty,
            3 => VhtChannelWidth::EightyPlusEighty,
            _ => return Err(Refused::VhtReserved),
        };
        Ok(Self { width, center_segment0, center_segment1, basic_mcs_map: u16::from_le_bytes([m0, m1]) })
    }
}
