//! HE Capabilities (802.11ax-2021 §9.4.2.248) and HE Operation (§9.4.2.249), Element ID Extension
//! 35 and 36. Their lengths follow from bits inside them, and any other length is refused.

use crate::read::Reader;
use crate::Refused;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeCapabilities<'a> {
    pub mac: [u8; 6],
    pub phy: [u8; 11],
    /// Rx and Tx HE-MCS maps for 80 MHz and narrower.
    pub mcs_80: [u8; 4],
    /// For 160 MHz, present when the Channel Width Set's bit 3 is.
    pub mcs_160: Option<[u8; 4]>,
    /// For 80+80 MHz, present when the Channel Width Set's bit 4 is.
    pub mcs_80p80: Option<[u8; 4]>,
    /// Present when the PHY capabilities' bit 23 is.
    pub ppe_thresholds: Option<&'a [u8]>,
}

impl<'a> HeCapabilities<'a> {
    pub(super) fn parse(body: &'a [u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(body);
        let length = |_| Refused::ElementLength;
        let mac = r.array().map_err(length)?;
        let phy: [u8; 11] = r.array().map_err(length)?;
        let [width_set, _, ppe_octet, ..] = phy;
        let mcs_80 = r.array().map_err(length)?;
        let mcs_160 = if width_set & 1 << 3 != 0 { Some(r.array().map_err(length)?) } else { None };
        let mcs_80p80 = if width_set & 1 << 4 != 0 { Some(r.array().map_err(length)?) } else { None };
        let ppe_thresholds = if ppe_octet & 1 << 7 != 0 { Some(ppe(&mut r)?) } else { None };
        if !r.is_empty() {
            return Err(Refused::ElementLength);
        }
        Ok(Self { mac, phy, mcs_80, mcs_160, mcs_80p80, ppe_thresholds })
    }
}

/// The PPE Thresholds field: NSTS and an RU index bitmask, then a pair of 3-bit thresholds per
/// stream and RU size, padded to whole octets (§9.4.2.248.5).
fn ppe<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], Refused> {
    let &first = r.rest().first().ok_or(Refused::PpeThresholds)?;
    let streams = u32::from(first & 0b111).saturating_add(1);
    let sizes = (first >> 3 & 0x0F).count_ones();
    let bits = streams.saturating_mul(sizes).saturating_mul(6).saturating_add(7);
    let octets = usize::try_from(bits.div_ceil(8)).map_err(|_| Refused::PpeThresholds)?;
    r.take(octets).map_err(|_| Refused::PpeThresholds)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeOperation {
    /// The HE Operation Parameters field, 24 bits.
    pub parameters: u32,
    /// BSS Color Information: the color in bits 0 to 5, Partial bit 6, Disabled bit 7.
    pub bss_color: u8,
    pub basic_mcs: u16,
    /// Channel Width and two center frequency segments, as VHT Operation's.
    pub vht: Option<[u8; 3]>,
    pub max_cohosted_bssid: Option<u8>,
    pub six_ghz: Option<SixGhz>,
}

/// The 6 GHz Operation Information field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SixGhz {
    pub primary_channel: u8,
    /// Channel Width in bits 0 and 1, Duplicate Beacon bit 2, Regulatory Info bits 3 to 5.
    pub control: u8,
    pub center_segment0: u8,
    pub center_segment1: u8,
    pub minimum_rate: u8,
}

impl HeOperation {
    const VHT_PRESENT: u32 = 1 << 14;
    const COHOSTED: u32 = 1 << 15;
    const SIX_GHZ_PRESENT: u32 = 1 << 17;

    pub(super) fn parse(body: &[u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(body);
        let length = |_| Refused::ElementLength;
        let [p0, p1, p2] = r.array().map_err(length)?;
        let parameters = u32::from_le_bytes([p0, p1, p2, 0]);
        let bss_color = r.u8().map_err(length)?;
        let basic_mcs = r.u16().map_err(length)?;
        let vht = if parameters & Self::VHT_PRESENT != 0 { Some(r.array().map_err(length)?) } else { None };
        let max_cohosted_bssid = if parameters & Self::COHOSTED != 0 { Some(r.u8().map_err(length)?) } else { None };
        let six_ghz = if parameters & Self::SIX_GHZ_PRESENT != 0 {
            let [primary_channel, control, center_segment0, center_segment1, minimum_rate] = r.array().map_err(length)?;
            Some(SixGhz { primary_channel, control, center_segment0, center_segment1, minimum_rate })
        } else {
            None
        };
        if !r.is_empty() {
            return Err(Refused::ElementLength);
        }
        Ok(Self { parameters, bss_color, basic_mcs, vht, max_cohosted_bssid, six_ghz })
    }
}
