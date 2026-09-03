//! ACPI table decoding, over the byte reader [`Phys`] asks the caller for.
//!
//! Input is firmware-supplied and untrusted: no input path panics, no walk
//! fails to terminate, and every refusal is a [`TableError`]. `tests/corpus.rs`
//! holds that claim; `tests/fixtures.rs` holds the decode against the tables
//! QEMU published to a real boot.
//!
//! Multi-byte fields are composed from bytes, little-endian, so no firmware
//! byte is transmuted into a type. Field offsets cite ACPI 6.5, except MCFG's
//! (PCI Firmware Specification) and HPET's (IA-PC HPET Specification).
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

mod fadt;
mod madt;

pub use fadt::{
    century_of, dsdt_address, iapc_boot_arch, rtc_century, Century, CMOS_RAM, FADT_PM1A_CNT_BLK,
    FADT_X_DSDT,
};
pub use madt::{
    madt_entries, IoApicEntry, MadtEntries, MadtEntry, MadtHalt, SourceOverride, MADT_ENTRIES,
};

/// Physical memory, as this decoder reads it.
///
/// # Contract
/// [`byte`](Phys::byte) is called only at an address a [`readable`](Phys::readable)
/// call accepted, within the length that call was given.
pub trait Phys: Copy {
    /// Whether `phys .. phys + len` may be read.
    fn readable(self, phys: u64, len: usize) -> bool;
    /// One byte at `phys`.
    fn byte(self, phys: u64) -> u8;
}

/// Why a firmware table cannot be used; each variant is a distinct instruction to the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableError {
    /// The RSDP UEFI handed us has the wrong signature or does not checksum.
    BadRsdp,
    /// An ACPI 1.0 root pointer, or a null XSDT address.
    // There is no RSDT fallback: this kernel is UEFI-only and every machine it targets publishes an XSDT.
    NoXsdt,
    /// No table with that signature in the XSDT.
    Absent,
    /// The declared length cannot hold the fields the caller reads, or is implausible.
    Length { declared: u32, needed: usize },
    /// The declared bytes do not sum to zero.
    // Which table failed isn't carried here: every call site already names the table in its own log line.
    Checksum,
    /// The table's declared bytes run off the end of what the reader can reach.
    Unmapped { at: u64, len: usize },
}

/// Largest length a table may declare — bounds the checksum walk and every derived entry count.
pub const MAX_TABLE_LEN: usize = 1024 * 1024;

/// An RSDP is 36 bytes; the bound stops the extended checksum from being pointed at the whole map.
const RSDP_MAX_LEN: usize = 64;
/// The ACPI 1.0 part of an RSDP, which checksums on its own (ACPI 6.5 §5.2.5.3).
const RSDP_V1_LEN: usize = 20;
/// ACPI 6.5 §5.2.5.3, Table 5.3: the whole RSDP, through `XsdtAddress` and the extended checksum.
const RSDP_LEN: usize = 36;
const RSDP_REVISION: usize = 15;
const RSDP_LENGTH: usize = 20;
const RSDP_XSDT_ADDRESS: usize = 24;

/// ACPI 6.5 §5.2.6, Table 5.4: every table starts with these 36 bytes.
pub const SDT_HEADER_LEN: usize = 36;
const SDT_SIGNATURE: usize = 0;
const SDT_LENGTH: usize = 4;
/// The header's revision byte, which the FADT's `X_` fields are gated on.
pub const SDT_REVISION: usize = 8;

/// A firmware table whose declared length has been checked to cover the read bytes, and whose declared bytes sum to zero.
#[derive(Clone, Copy)]
pub struct Table<P> {
    phys: P,
    base: u64,
    len: usize,
}

impl<P: Phys> Table<P> {
    /// Order matters: the checksum walk depends on the length already being bounded.
    pub fn open(
        phys: P,
        base: u64,
        signature: &[u8; 4],
        needed: usize,
    ) -> Result<Table<P>, TableError> {
        if !phys.readable(base, SDT_HEADER_LEN) {
            return Err(TableError::Unmapped { at: base, len: SDT_HEADER_LEN });
        }
        if bytes4(phys, base + SDT_SIGNATURE as u64) != *signature {
            return Err(TableError::Absent);
        }
        let declared = u32le(phys, base + SDT_LENGTH as u64);
        let len = declared as usize;
        let floor = needed.max(SDT_HEADER_LEN);
        if len < floor || len > MAX_TABLE_LEN {
            return Err(TableError::Length { declared, needed: floor });
        }
        if !phys.readable(base, len) {
            return Err(TableError::Unmapped { at: base, len });
        }
        if !sums_to_zero(phys, base, len) {
            return Err(TableError::Checksum);
        }
        Ok(Table { phys, base, len })
    }

    /// The declared length, already bounded by [`Table::open`].
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn phys(&self) -> P {
        self.phys
    }

    pub fn byte(&self, offset: usize) -> Option<u8> {
        (offset < self.len).then(|| self.phys.byte(self.base + offset as u64))
    }

    pub fn u16_at(&self, offset: usize) -> Option<u16> {
        self.fits(offset, 2).then(|| u16le(self.phys, self.base + offset as u64))
    }

    pub fn u32_at(&self, offset: usize) -> Option<u32> {
        self.fits(offset, 4).then(|| u32le(self.phys, self.base + offset as u64))
    }

    pub fn u64_at(&self, offset: usize) -> Option<u64> {
        self.fits(offset, 8).then(|| u64le(self.phys, self.base + offset as u64))
    }

    fn fits(&self, offset: usize, width: usize) -> bool {
        offset.checked_add(width).is_some_and(|end| end <= self.len)
    }
}

fn bytes4<P: Phys>(phys: P, at: u64) -> [u8; 4] {
    [phys.byte(at), phys.byte(at + 1), phys.byte(at + 2), phys.byte(at + 3)]
}

fn u16le<P: Phys>(phys: P, at: u64) -> u16 {
    u16::from(phys.byte(at)) | u16::from(phys.byte(at + 1)) << 8
}

fn u32le<P: Phys>(phys: P, at: u64) -> u32 {
    let mut v = 0u32;
    for i in 0..4 {
        v |= u32::from(phys.byte(at + i)) << (8 * i);
    }
    v
}

fn u64le<P: Phys>(phys: P, at: u64) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= u64::from(phys.byte(at + i)) << (8 * i);
    }
    v
}

/// Intact when the declared bytes sum to zero in 8 bits.
// `len` is bounded by the caller, and the range is one `readable` has accepted.
fn sums_to_zero<P: Phys>(phys: P, base: u64, len: usize) -> bool {
    let mut sum: u8 = 0;
    for i in 0..len as u64 {
        sum = sum.wrapping_add(phys.byte(base + i));
    }
    sum == 0
}

/// The XSDT, validated, from the RSDP UEFI handed the bootloader.
fn xsdt<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Table<P>, TableError> {
    if !phys.readable(rsdp_addr, RSDP_LEN) {
        return Err(TableError::BadRsdp);
    }
    for (i, want) in b"RSD PTR ".iter().enumerate() {
        if phys.byte(rsdp_addr + i as u64) != *want {
            return Err(TableError::BadRsdp);
        }
    }
    // The ACPI 1.0 part is a fixed 20 bytes; only after it checksums is the 2.0 extension read.
    if !sums_to_zero(phys, rsdp_addr, RSDP_V1_LEN) {
        return Err(TableError::BadRsdp);
    }

    if phys.byte(rsdp_addr + RSDP_REVISION as u64) < 2 {
        return Err(TableError::NoXsdt);
    }

    let declared = u32le(phys, rsdp_addr + RSDP_LENGTH as u64);
    let len = declared as usize;
    if !(RSDP_LEN..=RSDP_MAX_LEN).contains(&len) {
        return Err(TableError::Length { declared, needed: RSDP_LEN });
    }
    if !phys.readable(rsdp_addr, len) {
        return Err(TableError::Unmapped { at: rsdp_addr, len });
    }
    if !sums_to_zero(phys, rsdp_addr, len) {
        return Err(TableError::BadRsdp);
    }

    let address = u64le(phys, rsdp_addr + RSDP_XSDT_ADDRESS as u64);
    if !phys.readable(address, SDT_HEADER_LEN) {
        return Err(TableError::NoXsdt);
    }
    Table::open(phys, address, b"XSDT", SDT_HEADER_LEN)
}

/// The first table in the XSDT with this signature, validated for `needed` bytes.
// A second match with the same signature never replaces an invalid first.
pub fn find_table<P: Phys>(
    phys: P,
    rsdp_addr: u64,
    signature: &[u8; 4],
    needed: usize,
) -> Result<Table<P>, TableError> {
    let xsdt = xsdt(phys, rsdp_addr)?;
    // `Table::open` guarantees len >= SDT_HEADER_LEN, so this subtraction is total.
    let entry_count = (xsdt.len - SDT_HEADER_LEN) / 8;

    for i in 0..entry_count {
        let Some(at) = xsdt.u64_at(SDT_HEADER_LEN + i * 8) else { break };
        match Table::open(phys, at, signature, needed) {
            // An entry pointing at nothing is one entry skipped, not the end of the walk.
            Err(TableError::Absent | TableError::Unmapped { .. }) => continue,
            other => return other,
        }
    }
    Err(TableError::Absent)
}

/// PCI Firmware Specification 3.3, Table 4-3: the first allocation structure
/// sits one 8-byte reserved field past the header, and its base address is the
/// ECAM window's.
pub const MCFG_FIRST_ENTRY: usize = SDT_HEADER_LEN + 8;
const MCFG_ENTRY_LEN: usize = 16;

/// The ECAM base address the MCFG's first allocation structure names.
pub fn ecam_base<P: Phys>(phys: P, rsdp_addr: u64) -> Result<(Table<P>, u64), TableError> {
    let needed = MCFG_FIRST_ENTRY + MCFG_ENTRY_LEN;
    let mcfg = find_table(phys, rsdp_addr, b"MCFG", needed)?;
    let base = mcfg
        .u64_at(MCFG_FIRST_ENTRY)
        .ok_or(TableError::Length { declared: mcfg.len as u32, needed })?;
    Ok((mcfg, base))
}

/// IA-PC HPET Specification 1.0a, Table 3: the event timer block's Generic
/// Address Structure starts at 40 and its 64-bit address at 44.
const HPET_BASE_ADDRESS: usize = 44;

/// The HPET's MMIO base address.
pub fn hpet_base<P: Phys>(phys: P, rsdp_addr: u64) -> Result<u64, TableError> {
    let needed = HPET_BASE_ADDRESS + 8;
    let hpet = find_table(phys, rsdp_addr, b"HPET", needed)?;
    hpet.u64_at(HPET_BASE_ADDRESS)
        .ok_or(TableError::Length { declared: hpet.len as u32, needed })
}
