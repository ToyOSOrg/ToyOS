//! ACPI: the machine's half.
//!
//! The decode is `toyos-acpi`, pure and host-tested against QEMU's own tables
//! and a crafted corpus. What stays here is everything that touches the
//! machine: the direct-map reader the crate decodes through, the log lines a
//! machine owner reads a refusal off, and the `Vec`s the crate cannot allocate.
//!
//! All input is firmware-supplied and untrusted: no panic on any input path,
//! every failure is a [`TableError`] and the caller decides what it means.

use alloc::vec::Vec;
use core::mem::size_of;
use core::ptr::{read_unaligned, read_volatile};
use crate::log;
use crate::DirectMap;
use toyos_acpi::{Century, MadtEntry, Mapped, Memory, CMOS_RAM, MADT_ENTRIES, SDT_HEADER_LEN, SDT_REVISION};

pub use toyos_acpi::{IoApicEntry, SourceOverride, TableError};

pub struct MadtInfo {
    pub apic_ids: Vec<u32>,
    pub io_apics: Vec<IoApicEntry>,
    pub source_overrides: Vec<SourceOverride>,
}

/// Firmware's physical addresses, read through the direct map.
#[derive(Clone, Copy)]
pub struct DirectMemory;

impl Memory for DirectMemory {
    fn byte(self, phys: u64) -> u8 {
        // SAFETY: `Mapped` asks only inside the direct map's extent it was
        // made with, and the map never shrinks.
        unsafe { read_volatile(DirectMap::from_phys(phys).as_ptr::<u8>()) }
    }
}

type DirectPhys = Mapped<DirectMemory>;

/// The reader over the direct map as far as it reaches now.
pub fn direct_phys() -> DirectPhys {
    Mapped::new(DirectMemory, crate::mm::direct_map_end())
}

/// A firmware table whose declared length has been checked to cover the read bytes, and whose declared bytes sum to zero.
#[derive(Clone, Copy)]
pub struct Table(toyos_acpi::Table<DirectPhys>);

impl Table {
    /// The declared length, already bounded by [`find_table`].
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Where firmware put it, for an inventory a machine owner compares boots by.
    pub fn base(&self) -> u64 {
        self.0.base()
    }

    /// A copy of the `T` at `offset`, or `None` when the table is not that long.
    // A copy, not a reference: `&T` would assert all of `T` is valid memory — exactly the claim being checked for a struct whose tail may run past the declared length.
    pub fn field<T: Copy>(&self, offset: usize) -> Option<T> {
        let end = offset.checked_add(size_of::<T>())?;
        if end > self.0.len() {
            return None;
        }
        let at = self.0.base() + offset as u64;
        // SAFETY: `Table::open` put the whole declared length inside the direct map, and `end <= len` puts this read inside it.
        Some(unsafe { read_unaligned(DirectMap::from_phys(at).as_ptr::<u8>().cast::<T>()) })
    }
}

/// The first table in the XSDT with this signature, validated for `needed` bytes.
pub fn find_table(rsdp_addr: u64, signature: &[u8; 4], needed: usize) -> Result<Table, TableError> {
    toyos_acpi::find_table(direct_phys(), rsdp_addr, signature, needed).map(Table)
}

/// ACPI 6.5 §5.2.6, Table 5.4: OEM ID is six bytes at offset 10 of every table header.
const SDT_OEM_ID: usize = 10;

/// Every table this kernel goes on to read, named once with what its own header
/// declares.
///
/// **Presence is not the claim, validation is.** [`find_table`] reaches a table
/// only through an RSDP whose 1.0 and extended checksums both hold and an XSDT
/// whose own does, and hands one back only when its declared bytes sum to zero
/// — so a row here is a table that checksummed, and the line that closes the
/// list is the machine's answer to how many of them it has.
pub fn inventory(rsdp_addr: u64) {
    // The tables this architecture decodes.
    const READ: &[&[u8; 4]] = crate::arch::boot::ACPI_TABLES;

    let mut validated = 0usize;
    for signature in READ {
        let name = core::str::from_utf8(*signature).unwrap_or("????");
        match find_table(rsdp_addr, signature, SDT_HEADER_LEN) {
            Ok(table) => {
                validated += 1;
                let oem: [u8; 6] = table.field(SDT_OEM_ID).unwrap_or_default();
                let revision: u8 = table.field(SDT_REVISION).unwrap_or_default();
                log!(
                    "ACPI: {name} at {:#x} len={} rev={revision} oem={:?} checksummed",
                    table.base(),
                    table.len(),
                    core::str::from_utf8(&oem).unwrap_or("<not ascii>").trim_end(),
                );
            }
            Err(e) => log!("ACPI: {name} not read: {e:?}"),
        }
    }
    log!(
        "ACPI: {validated} of {} tables checksummed under the RSDP at {rsdp_addr:#x}",
        READ.len()
    );
}

/// Log a refusal with the reason, and hand the caller `None`.
// Never a panic: a machine owner needs to see the reason, not have the kernel die on a firmware defect.
fn refuse<T>(what: &str, error: TableError) -> Option<T> {
    log!("ACPI: {what} unusable: {error:?}");
    None
}

/// Given the RSDP address from UEFI, parse XSDT -> MCFG -> return the ECAM
/// base address and the PCI segment group it serves.
pub fn find_ecam_base(rsdp_addr: u64) -> Option<(u64, u16)> {
    log!("ACPI: RSDP at {rsdp_addr:#x}");
    let (mcfg, base) = match toyos_acpi::ecam_base(direct_phys(), rsdp_addr) {
        Ok(found) => found,
        Err(e) => return refuse("MCFG", e),
    };
    // PCI Firmware Specification 3.3, Table 4-3: the entry's segment group
    // follows its base, inside the entry `ecam_base` already bounded.
    let segment = mcfg
        .u16_at(toyos_acpi::MCFG_FIRST_ENTRY + 8)
        .expect("ecam_base bounded the whole first allocation structure");
    log!("ACPI: MCFG found at {:#x}", mcfg.base());
    log!("ACPI: ECAM base address: {base:#x}");
    Some((base, segment))
}

/// FADT revision and the IA-PC boot architecture flags.
// `Err` is not "absent" and must not be treated as one by the caller.
pub fn iapc_boot_arch(rsdp_addr: u64) -> Result<(u8, u16), TableError> {
    toyos_acpi::iapc_boot_arch(direct_phys(), rsdp_addr)
}

/// Which CMOS register holds the RTC's century, as the FADT names it.
// `Ok(None)` is "no century register", distinct from `Err`, which the caller must not treat as one.
pub fn rtc_century_register(rsdp_addr: u64) -> Result<Option<u8>, TableError> {
    let named = toyos_acpi::rtc_century(direct_phys(), rsdp_addr)?;
    match named {
        Century::Absent => {
            log!("ACPI: the FADT names no RTC century register");
            Ok(None)
        }
        Century::OutOfRange(index) => {
            log!(
                "ACPI: the FADT puts the RTC century register at CMOS {index:#04x}, outside {:#04x}..={:#04x} — ignoring it",
                CMOS_RAM.start(),
                CMOS_RAM.end()
            );
            Ok(None)
        }
        Century::At(index) => {
            log!("ACPI: the FADT puts the RTC century register at CMOS {index:#04x}");
            Ok(Some(index))
        }
    }
}

/// Given the RSDP address, parse XSDT -> HPET table -> return HPET MMIO base address.
pub fn find_hpet_base(rsdp_addr: u64) -> Option<u64> {
    let base = match toyos_acpi::hpet_base(direct_phys(), rsdp_addr) {
        Ok(base) => base,
        Err(e) => return refuse("HPET", e),
    };
    log!("ACPI: HPET at {base:#x}");
    Some(base)
}

/// Parse MADT (signature "APIC") to discover per-CPU APIC IDs.
pub fn parse_madt(rsdp_addr: u64) -> Option<MadtInfo> {
    let madt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"APIC", MADT_ENTRIES) {
        Ok(table) => table,
        Err(e) => return refuse("MADT", e),
    };

    let mut apic_ids = Vec::new();
    let mut io_apics = Vec::new();
    let mut source_overrides = Vec::new();

    for item in toyos_acpi::madt_entries(&madt) {
        match item {
            Ok(MadtEntry::LocalApic { apic_id, enabled }) => {
                if enabled {
                    apic_ids.push(apic_id);
                }
            }
            Ok(MadtEntry::IoApic(entry)) => io_apics.push(entry),
            Ok(MadtEntry::SourceOverride(entry)) => source_overrides.push(entry),
            // A GIC structure on a machine this walk reads APICs from is as
            // foreign to it as a type it does not know.
            Ok(MadtEntry::Gicc(_)
            | MadtEntry::Gicd { .. }
            | MadtEntry::Gicr { .. }
            | MadtEntry::Its { .. }
            | MadtEntry::Other(_)) => {}
            Err(halt) => {
                log!(
                    "ACPI: MADT entry at +{} declares {} bytes of a {}-byte list — stopping",
                    halt.at,
                    halt.declared,
                    halt.list_len
                );
                break;
            }
        }
    }

    log!("ACPI: MADT cpus={:?}", apic_ids);
    Some(MadtInfo { apic_ids, io_apics, source_overrides })
}
