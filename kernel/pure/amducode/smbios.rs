//! The firmware's own account of itself in SMBIOS (DMTF DSP0134): the entry
//! point the UEFI configuration table names, the structure table it points
//! at, and the BIOS Information structure (type 0) in that table.
//!
//! Every byte is firmware's and untrusted: a malformed entry point is
//! refused, and a structure table is read only as far as it is well formed.

use core::fmt;

/// DSP0134 §5.2.1, the SMBIOS 2.1 (32-bit) entry point.
const ANCHOR_2: &[u8] = b"_SM_";
const INTERMEDIATE: &[u8] = b"_DMI_";
/// DSP0134 §5.2.2, the SMBIOS 3.0 (64-bit) entry point.
const ANCHOR_3: &[u8] = b"_SM3_";
/// The largest structure table this reads, whatever an entry point declares.
pub const MAX_TABLE_LEN: u32 = 1 << 20;
const END_OF_TABLE: u8 = 127;
const BIOS_INFORMATION: u8 = 0;

/// Where the structure table is, and the specification version it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryPoint {
    pub major: u8,
    pub minor: u8,
    /// The table's physical address.
    pub table: u64,
    /// Its length (2.1), or the most it may be (3.0), at most [`MAX_TABLE_LEN`].
    pub len: u32,
}

/// The longest entry point, which is as much as [`entry_point`] reads.
pub const ENTRY_POINT_MAX: usize = 0x1F;

/// The entry point in `bytes`, which start at the address the configuration
/// table names; `Err` says why it is not one.
pub fn entry_point(bytes: &[u8]) -> Result<EntryPoint, &'static str> {
    let byte = |at: usize| bytes.get(at).copied().ok_or("the entry point is cut short");
    let sums = |len: usize| -> Result<(), &'static str> {
        let covered = bytes.get(..len).ok_or("the entry point is cut short")?;
        match covered.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) {
            0 => Ok(()),
            _ => Err("the entry point's checksum does not hold"),
        }
    };
    if bytes.starts_with(ANCHOR_3) {
        let len = usize::from(byte(0x06)?);
        if len < 0x18 {
            return Err("a 3.0 entry point shorter than 0x18 bytes");
        }
        sums(len)?;
        let max = u32::from_le_bytes([byte(0x0C)?, byte(0x0D)?, byte(0x0E)?, byte(0x0F)?]);
        let table = bytes.get(0x10..0x18).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes);
        let table = table.ok_or("the entry point is cut short")?;
        return Ok(EntryPoint { major: byte(0x07)?, minor: byte(0x08)?, table, len: max.min(MAX_TABLE_LEN) });
    }
    if bytes.starts_with(ANCHOR_2) {
        let len = usize::from(byte(0x05)?);
        if len < 0x1F {
            return Err("a 2.1 entry point shorter than 0x1F bytes");
        }
        sums(len)?;
        if bytes.get(0x10..0x15) != Some(INTERMEDIATE) {
            return Err("a 2.1 entry point without its intermediate anchor");
        }
        let intermediate = bytes.get(0x10..0x1F).ok_or("the entry point is cut short")?;
        if intermediate.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) != 0 {
            return Err("the intermediate checksum does not hold");
        }
        let len = u16::from_le_bytes([byte(0x16)?, byte(0x17)?]);
        let table = u32::from_le_bytes([byte(0x18)?, byte(0x19)?, byte(0x1A)?, byte(0x1B)?]);
        return Ok(EntryPoint { major: byte(0x06)?, minor: byte(0x07)?, table: u64::from(table), len: u32::from(len) });
    }
    Err("no SMBIOS anchor")
}

/// One structure: its type, its formatted area (header included) and its
/// strings, each without its terminating NUL.
#[derive(Clone, Copy, Debug)]
pub struct Structure<'a> {
    pub kind: u8,
    pub formatted: &'a [u8],
    strings: &'a [u8],
}

impl<'a> Structure<'a> {
    /// Every string in the structure's set, in order.
    pub fn strings(&self) -> impl Iterator<Item = &'a [u8]> {
        self.strings.split(|&b| b == 0).filter(|s| !s.is_empty())
    }

    /// The string the byte at `offset` numbers, from 1; `None` for 0, which
    /// is no string, or a number past the set.
    pub fn string(&self, offset: usize) -> Option<&'a [u8]> {
        let n = usize::from(*self.formatted.get(offset)?);
        n.checked_sub(1).and_then(|n| self.strings().nth(n))
    }
}

/// Every well-formed structure in `table`, up to the end-of-table structure
/// or the first malformed one.
pub fn structures(table: &[u8]) -> impl Iterator<Item = Structure<'_>> {
    let mut rest = table;
    core::iter::from_fn(move || {
        let len = usize::from(*rest.get(1)?);
        if len < 4 {
            return None;
        }
        let formatted = rest.get(..len)?;
        let set = rest.get(len..)?;
        // The set ends in two NULs; a structure with no strings is two NULs alone.
        let end = set.windows(2).position(|pair| pair == [0, 0])?;
        let structure = Structure { kind: formatted[0], formatted, strings: &set[..end] };
        rest = if structure.kind == END_OF_TABLE { &[] } else { &set[end + 2..] };
        Some(structure)
    })
}

/// The BIOS Information structure's identifying fields (DSP0134 §7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bios<'a> {
    pub vendor: Option<&'a [u8]>,
    pub version: Option<&'a [u8]>,
    pub date: Option<&'a [u8]>,
    /// System BIOS major and minor release (2.4 and later).
    pub release: Option<(u8, u8)>,
    /// Embedded controller firmware major and minor release (2.4 and later).
    pub ec: Option<(u8, u8)>,
}

/// The first type 0 structure in `table`.
pub fn bios(table: &[u8]) -> Option<Bios<'_>> {
    let s = structures(table).find(|s| s.kind == BIOS_INFORMATION)?;
    let pair = |at: usize| Some((*s.formatted.get(at)?, *s.formatted.get(at + 1)?));
    Some(Bios { vendor: s.string(0x04), version: s.string(0x05), date: s.string(0x08), release: pair(0x14), ec: pair(0x16) })
}

/// Every string in `table` that names an AGESA or Platform Initialization
/// version, as firmware that shows one spells it: `AGESA`, or a word ending
/// in `PI`, four letters or digits at least, before a space or a dash (`ComboAM4v2PI 1.2.0.E`, `RenoirPI-FP6`).
pub fn platform_versions(table: &[u8]) -> impl Iterator<Item = &[u8]> {
    structures(table).flat_map(|s| s.strings()).filter(|s| names_platform(s))
}

fn names_platform(s: &[u8]) -> bool {
    s.windows(5).any(|w| w == b"AGESA")
        || s.windows(3).enumerate().any(|(at, w)| {
            w[..2] == *b"PI"
                && matches!(w[2], b' ' | b'-')
                && at >= 4
                && s[at - 4..at].iter().all(u8::is_ascii_alphanumeric)
        })
}

/// Firmware's bytes as a quoted string, every byte outside printable ASCII
/// escaped, so a line carries nothing a reader of the log can be fooled by.
pub struct Quoted<'a>(pub Option<&'a [u8]>);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(bytes) = self.0 else { return f.write_str("none") };
        f.write_str("\"")?;
        for &b in bytes {
            match b {
                b'"' | b'\\' => write!(f, "\\{}", char::from(b))?,
                0x20..=0x7E => write!(f, "{}", char::from(b))?,
                _ => write!(f, "\\x{b:02x}")?,
            }
        }
        f.write_str("\"")
    }
}
