//! BlockAckReq (§9.3.1.7) and BlockAck (§9.3.1.8) of the Compressed variant, the only one an HT or
//! VHT station's immediate block-ack agreement uses; their bodies follow Frame Control and
//! Duration.

use crate::read::Reader;
use crate::{Address, Refused, Seq};

/// The BAR and BA Type value of the Compressed variant (Table 9-24, Table 9-28).
const COMPRESSED: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockAckReq {
    pub ra: Address,
    pub ta: Address,
    pub tid: u8,
    /// The Starting Sequence Number: everything before it is no longer to be waited for.
    pub ssn: Seq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockAck {
    pub ra: Address,
    pub ta: Address,
    pub tid: u8,
    pub ssn: Seq,
    /// Bit n acknowledges sequence number SSN + n.
    pub bitmap: u64,
}

impl BlockAck {
    /// Whether `seq` is inside the bitmap and acknowledged by it.
    pub const fn acked(&self, seq: Seq) -> bool {
        let offset = seq.since(self.ssn);
        offset < 64 && self.bitmap >> offset & 1 != 0
    }
}

/// The receiver and transmitter, the control field's TID, and the Starting Sequence Control.
fn compressed(r: &mut Reader<'_>) -> Result<(Address, Address, u8, Seq), Refused> {
    let ra = r.address()?;
    // A VHT transmitter may set the TA's group bit as a bandwidth signaling TA (§9.3.1.1).
    let Address([first, rest @ ..]) = r.address()?;
    let [b1, b2, b3, b4, b5] = rest;
    let ta = Address([first & !1, b1, b2, b3, b4, b5]);
    let control = r.u16()?;
    if (control >> 1) & 0x0F != COMPRESSED {
        return Err(Refused::BlockAckVariant);
    }
    let [_, high] = control.to_le_bytes();
    let ssc = r.u16()?;
    // A nonzero Fragment Number in a Compressed BlockAck sizes a bitmap past 64 bits, which the
    // HE amendment defines and 802.11-2020 does not.
    if ssc & 0x000F != 0 {
        return Err(Refused::BitmapSize);
    }
    Ok((ra, ta, high >> 4, Seq::new(ssc >> 4)))
}

impl BlockAckReq {
    pub(crate) fn parse(after_duration: &[u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(after_duration);
        let (ra, ta, tid, ssn) = compressed(&mut r)?;
        if !r.is_empty() {
            return Err(Refused::ControlLength);
        }
        Ok(Self { ra, ta, tid, ssn })
    }
}

impl BlockAck {
    pub(crate) fn parse(after_duration: &[u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(after_duration);
        let (ra, ta, tid, ssn) = compressed(&mut r)?;
        let bitmap = r.u64()?;
        if !r.is_empty() {
            return Err(Refused::ControlLength);
        }
        Ok(Self { ra, ta, tid, ssn, bitmap })
    }
}
