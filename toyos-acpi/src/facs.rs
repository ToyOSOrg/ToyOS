//! The FACS (ACPI 6.5 §5.2.10, Table 5.13), which the FADT points at and no
//! XSDT lists, and the Global Lock in it (§5.2.10.1, Table 5.16).
//!
//! The lock is one dword both the operating system and the firmware's SMI
//! handlers change by compare-and-exchange. [`acquire`] and [`release`] are
//! the two transitions §5.2.10.1 gives as code, each from the word read to the
//! word to exchange it for.

use crate::fadt::{FADT_FIRMWARE_CTRL, FADT_X_FIRMWARE_CTRL};
use crate::{Phys, Table, MAX_TABLE_LEN, SDT_REVISION};

/// Table 5.13: `Length` at 4, the Global Lock at 16, in a structure of 64
/// bytes or more.
const FACS_LENGTH: u64 = 4;
pub const FACS_GLOBAL_LOCK: u64 = 16;
const FACS_MIN_LEN: u32 = 64;

/// Table 5.16.
pub const PENDING: u32 = 1 << 0;
pub const OWNED: u32 = 1 << 1;

/// Where the FACS is, and the length it declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Facs {
    pub base: u64,
    pub len: u32,
}

/// Why a machine's FACS is none this decoder hands out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FacsRefused {
    /// The FADT names none: both address fields are zero.
    Absent,
    /// Its first 64 bytes are not all readable.
    Unmapped(u64),
    /// §5.2.10 aligns it on 64 bytes; one not on four has no lock word a
    /// compare-and-exchange can address.
    Misaligned(u64),
    Signature,
    Length(u32),
}

/// The FACS the FADT names: `X_FIRMWARE_CTRL` where a revision that has it
/// holds a non-zero one, else `FIRMWARE_CTRL` (Table 5.9).
pub fn facs<P: Phys>(phys: P, fadt: &Table<P>) -> Result<Facs, FacsRefused> {
    let wide = match fadt.byte(SDT_REVISION) {
        Some(r) if r >= 2 => fadt.u64_at(FADT_X_FIRMWARE_CTRL).filter(|a| *a != 0),
        _ => None,
    };
    let base = wide.unwrap_or_else(|| u64::from(fadt.u32_at(FADT_FIRMWARE_CTRL).unwrap_or(0)));
    if base == 0 {
        return Err(FacsRefused::Absent);
    }
    if !base.is_multiple_of(4) {
        return Err(FacsRefused::Misaligned(base));
    }
    if !phys.readable(base, FACS_MIN_LEN as usize) {
        return Err(FacsRefused::Unmapped(base));
    }
    if crate::bytes4(phys, base) != *b"FACS" {
        return Err(FacsRefused::Signature);
    }
    let len = crate::u32le(phys, base + FACS_LENGTH);
    if len < FACS_MIN_LEN || len as usize > MAX_TABLE_LEN {
        return Err(FacsRefused::Length(len));
    }
    Ok(Facs { base, len })
}

/// §5.2.10.1's `AcquireGlobalLock`: the word to exchange `word` for, and
/// whether that exchange takes the lock. Where it does not, the new word
/// carries the pending bit, and the owner signals its release. Decided by the
/// owner bit read, where the sequence's `cmp dl, 3` reads the low byte whole:
/// the two agree while bits 2 to 7 are zero, as Table 5.16 reserves them.
pub const fn acquire(word: u32) -> (u32, bool) {
    let owned = word & OWNED != 0;
    let new = word & !PENDING | OWNED | if owned { PENDING } else { 0 };
    (new, !owned)
}

/// §5.2.10.1's `ReleaseGlobalLock`: the word to exchange `word` for, and
/// whether the other side asked while the lock was held and is owed the
/// release's signal.
pub const fn release(word: u32) -> (u32, bool) {
    (word & !(PENDING | OWNED), word & PENDING != 0)
}
