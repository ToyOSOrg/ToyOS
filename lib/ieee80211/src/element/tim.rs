//! The TIM element (9.4.2.5).

use crate::read::Reader;
use crate::Refused;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tim<'a> {
    pub dtim_count: u8,
    /// Never 0.
    pub dtim_period: u8,
    control: u8,
    bitmap: &'a [u8],
}

/// The partial virtual bitmap covers AIDs 0 to 2007: octets 0 to 250.
const BITMAP_OCTETS: usize = 251;

impl<'a> Tim<'a> {
    pub(super) fn parse(body: &'a [u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(body);
        let [dtim_count, dtim_period, control] = r.array().map_err(|_| Refused::ElementLength)?;
        let bitmap = r.rest();
        if bitmap.is_empty() {
            return Err(Refused::ElementLength);
        }
        if dtim_period == 0 {
            return Err(Refused::DtimPeriod);
        }
        let tim = Self { dtim_count, dtim_period, control, bitmap };
        if tim.offset().saturating_add(bitmap.len()) > BITMAP_OCTETS {
            return Err(Refused::TimBitmap);
        }
        Ok(tim)
    }

    /// N1, the first octet the partial bitmap holds: the Bitmap Offset subfield times two.
    fn offset(&self) -> usize {
        usize::from(self.control & 0xFE)
    }

    /// Whether group-addressed frames are buffered: bit 0 of the Bitmap Control field.
    pub const fn group_buffered(&self) -> bool {
        self.control & 1 != 0
    }

    /// Whether frames are buffered for the station with association ID `aid`.
    pub fn buffered(&self, aid: u16) -> bool {
        let aid = usize::from(aid);
        (aid / 8)
            .checked_sub(self.offset())
            .and_then(|octet| self.bitmap.get(octet))
            .is_some_and(|octet| octet >> (aid % 8) & 1 != 0)
    }
}
