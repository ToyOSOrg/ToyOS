//! Interrupt remapping: the table a unit walks to decide where an interrupt
//! message lands.
//!
//! Every layout here is quoted from a text this project did not write — Intel
//! VT-d Rev. 4.0, order number D51397-015:
//!
//! - **`IRTA_REG`**, Section 11.4.10: table address bits 63:12, `EIME` bit 11,
//!   `S` bits 3:0 holding `2^(S+1)` entries.
//! - **`GCMD`/`GSTS`**, Sections 11.4.4.1 and 11.4.4.2: `IRE`/`IRES` bit 25,
//!   `SIRTP`/`IRTPS` bit 24, `CFI`/`CFIS` bit 23.
//!
//! One table serves every unit, which Section 5.1.3 permits explicitly, so an
//! index names the same interrupt whichever unit walks it.
//!
//! `TABLES` is taken before this module's lock and never after it.

use alloc::vec::Vec;

use crate::iommu::StreamId;
use crate::sync::Lock;

use super::table::{Table, Tables};

pub const IRTA_REG: u64 = 0xB8;

/// `GCMD.IRE`; `GSTS.IRES` confirms it at the same bit position.
pub const INTERRUPT_REMAPPING_ENABLE: u32 = 1 << 25;
/// `GCMD.SIRTP`; `GSTS.IRTPS` confirms it at the same bit position.
pub const SET_TABLE_POINTER: u32 = 1 << 24;
/// `GSTS.CFIS`. Set means compatibility-format messages bypass remapping; this
/// kernel never writes `GCMD.CFI`, so it reads clear and they are blocked.
pub const COMPATIBILITY_FORMAT: u32 = 1 << 23;

/// `2^(7+1)` entries of 128 bits is exactly the 4 KiB [`Tables::alloc`] hands out.
const SIZE_FIELD: u64 = 7;

const EXTENDED_INTERRUPT_MODE: u64 = 1 << 11;

/// The one table, and what every unit was told about it.
struct Remap {
    /// `None` until a unit is armed, which is also what "no source may use the remappable format" means.
    table: Option<Table>,
    /// `ECAP.EIM` on every unit. Clear bounds a destination to the eight bits `DST` then holds.
    extended: bool,
    /// Requester ids firmware gave this machine's I/O APICs, which Section 8.3.1.1 requires it to name.
    apics: Vec<(u8, StreamId)>,
}

static REMAP: Lock<Remap> = Lock::new(Remap { table: None, extended: false, apics: Vec::new() });

/// Record the requester id a DMAR device scope gave the I/O APIC with this id.
pub fn describe_apic(apic_id: u8, source: StreamId) {
    REMAP.lock().apics.push((apic_id, source));
}

/// Whether firmware named a requester id for every one of `apics`.
pub fn apics_are_named(apics: &[u8]) -> bool {
    let remap = REMAP.lock();
    apics.iter().all(|id| remap.apics.iter().any(|(named, _)| named == id))
}

/// Allocate the shared table on first ask and return the value `IRTA_REG` takes for it.
pub fn arm(tables: &mut Tables, extended: bool) -> u64 {
    let mut remap = REMAP.lock();
    remap.extended = extended;
    let table = match remap.table {
        Some(table) => table,
        None => *remap.table.insert(tables.alloc()),
    };
    table.phys() | if extended { EXTENDED_INTERRUPT_MODE } else { 0 } | SIZE_FIELD
}

/// The table's physical address, for the line reporting what a unit was pointed at.
pub fn table_address() -> u64 {
    REMAP.lock().table.map_or(0, Table::phys)
}
