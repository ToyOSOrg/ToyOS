//! The DSDT (signature `DSDT`), read for one value by a byte scan, not an AML
//! interpreter: the first element of its `\_S5_` package (ACPI 6.5, "\_Sx").

use crate::{Phys, Table, SDT_HEADER_LEN};

/// AML's `PackageOp` and `BytePrefix` (ACPI 6.5, "AML Grammar Definition").
const PACKAGE_OP: u8 = 0x12;
const BYTE_PREFIX: u8 = 0x0A;

/// The widest `SLP_TYPx` the PM1 control register holds: three bits, 12:10.
const SLP_TYP_MAX: u8 = 7;

/// What a DSDT says to write to `SLP_TYPa` for S5 soft-off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum S5 {
    SlpTyp(u8),
    /// No `_S5_` followed by `PackageOp` whose first element lies inside the
    /// table.
    Absent,
    /// A first element wider than `SLP_TYPx`, which shifted into place would
    /// set `SLP_EN` and reserved bits.
    Wide(u8),
}

/// The first element of the first `\_S5_` package in `dsdt`, read inside the
/// table's declared length: a `BytePrefix` constant, or the element's own
/// byte, which is `ZeroOp` and `OneOp`'s value.
pub fn s5_slp_typ<P: Phys>(dsdt: &Table<P>) -> S5 {
    for at in SDT_HEADER_LEN..dsdt.len() {
        if (0..4).any(|i| dsdt.byte(at + i) != Some(b"_S5_"[i])) || dsdt.byte(at + 4) != Some(PACKAGE_OP) {
            continue;
        }
        // `PkgLength`'s lead byte counts the bytes after it in bits 7:6
        // ("Package Length Encoding"); `NumElements` follows it.
        let Some(lead) = dsdt.byte(at + 5) else { return S5::Absent };
        let element = at + 5 + 1 + usize::from(lead >> 6) + 1;
        let value = match dsdt.byte(element) {
            Some(BYTE_PREFIX) => dsdt.byte(element + 1),
            byte => byte,
        };
        return match value {
            Some(value) if value <= SLP_TYP_MAX => S5::SlpTyp(value),
            Some(value) => S5::Wide(value),
            None => S5::Absent,
        };
    }
    S5::Absent
}
