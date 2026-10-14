//! BlockAckReq (802.11-2020 §9.3.1.7) and BlockAck (§9.3.1.8) of the Compressed variant, the one an
//! HT, VHT or HE station's immediate block-ack agreement uses; their bodies follow Frame Control
//! and Duration.
//!
//! An HE BlockAck's bitmap is 32, 64, 128 or 256 bits long, as its Fragment Number subfield says
//! (802.11ax-2021 §9.3.1.8.2). Acknowledging fragments, the subfield's bit 0, is refused, because a
//! fragment is (`Refused::Fragmented`).

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
    /// Bit n acknowledges sequence number SSN + n, for n below `bits`.
    pub bitmap: [u64; 4],
    pub bits: u16,
}

impl BlockAck {
    /// Whether `seq` is inside the bitmap and acknowledged by it.
    pub fn acked(&self, seq: Seq) -> bool {
        let offset = seq.since(self.ssn);
        offset < self.bits
            && self
                .bitmap
                .get(usize::from(offset / 64))
                .is_some_and(|word| word >> (offset % 64) & 1 != 0)
    }
}

/// The receiver and transmitter, the control field's TID, and the Starting Sequence Control's
/// Fragment Number and sequence number.
fn compressed(r: &mut Reader<'_>) -> Result<(Address, Address, u8, u8, Seq), Refused> {
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
    let [low, seq_high] = r.u16()?.to_le_bytes();
    let seq = Seq::new(u16::from_le_bytes([low, seq_high]) >> 4);
    Ok((ra, ta, high >> 4, low & 0x0F, seq))
}

impl BlockAckReq {
    pub(crate) fn parse(after_duration: &[u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(after_duration);
        let (ra, ta, tid, fragment, ssn) = compressed(&mut r)?;
        if fragment != 0 {
            return Err(Refused::BitmapSize);
        }
        if !r.is_empty() {
            return Err(Refused::ControlLength);
        }
        Ok(Self { ra, ta, tid, ssn })
    }
}

impl BlockAck {
    pub(crate) fn parse(after_duration: &[u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(after_duration);
        let (ra, ta, tid, fragment, ssn) = compressed(&mut r)?;
        // Bits 2 and 1 of the Fragment Number give the bitmap's length, bit 0 a fragment level.
        let bits: u16 = match fragment {
            0b0000 => 64,
            0b0010 => 256,
            0b0100 => 128,
            0b0110 => 32,
            _ => return Err(Refused::BitmapSize),
        };
        let mut bitmap = [0u64; 4];
        let mut octets = r.take(usize::from(bits / 8))?.iter();
        for word in &mut bitmap {
            let mut value = [0u8; 8];
            for (slot, &octet) in value.iter_mut().zip(octets.by_ref()) {
                *slot = octet;
            }
            *word = u64::from_le_bytes(value);
        }
        if !r.is_empty() {
            return Err(Refused::ControlLength);
        }
        Ok(Self { ra, ta, tid, ssn, bitmap, bits })
    }
}
