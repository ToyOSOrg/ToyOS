//! The WMM Information and Parameter elements (Wi-Fi Alliance, WMM Specification v1.2.0, §2.2.1
//! and §2.2.2): vendor-specific elements under OUI 00-50-F2, OUI type 2, version 1.

use crate::read::Reader;
use crate::Refused;

/// The OUI and OUI type a WMM element's body starts with.
pub(super) const HEADER: [u8; 4] = [0x00, 0x50, 0xF2, 0x02];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wmm {
    Information { qos_info: u8 },
    Parameter { qos_info: u8, access_categories: [AcParameters; 4] },
}

/// One AC Parameter Record, indexed in [`Wmm::Parameter`] by its ACI: best effort, background,
/// video, voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcParameters {
    /// 2 to 15 slots.
    pub aifsn: u8,
    pub admission_control: bool,
    /// The exponents of CWmin and CWmax: CW is 2^ECW − 1.
    pub ecw_min: u8,
    pub ecw_max: u8,
    /// In units of 32 µs.
    pub txop_limit: u16,
}

const INFORMATION: u8 = 0;
const PARAMETER: u8 = 1;

impl Wmm {
    /// The element after its OUI and OUI type, or `None` for another WMM subtype (a TSPEC).
    pub(super) fn parse(after_header: &[u8]) -> Result<Option<Self>, Refused> {
        let mut r = Reader::new(after_header);
        let length = |_| Refused::ElementLength;
        let subtype = r.u8().map_err(length)?;
        if subtype != INFORMATION && subtype != PARAMETER {
            return Ok(None);
        }
        if r.u8().map_err(length)? != 1 {
            return Err(Refused::WmmVersion);
        }
        let qos_info = r.u8().map_err(length)?;
        let wmm = if subtype == INFORMATION {
            Self::Information { qos_info }
        } else {
            r.u8().map_err(length)?;
            let mut access_categories = [None; 4];
            for _ in 0..4 {
                let [aci_aifsn, ecw] = r.array().map_err(length)?;
                let txop_limit = r.u16().map_err(length)?;
                let aifsn = aci_aifsn & 0x0F;
                if aifsn < 2 {
                    return Err(Refused::WmmAifsn);
                }
                let record = AcParameters {
                    aifsn,
                    admission_control: aci_aifsn & 1 << 4 != 0,
                    ecw_min: ecw & 0x0F,
                    ecw_max: ecw >> 4,
                    txop_limit,
                };
                if let Some(slot) = access_categories.get_mut(usize::from(aci_aifsn >> 5 & 0b11)) {
                    *slot = Some(record);
                }
            }
            // Four records fill all four categories only if no ACI repeats.
            let [Some(be), Some(bk), Some(vi), Some(vo)] = access_categories else {
                return Err(Refused::WmmAccessCategory);
            };
            Self::Parameter { qos_info, access_categories: [be, bk, vi, vo] }
        };
        if !r.is_empty() {
            return Err(Refused::ElementLength);
        }
        Ok(Some(wmm))
    }
}
