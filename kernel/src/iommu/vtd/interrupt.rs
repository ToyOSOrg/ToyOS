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
//! - **IRTE**, Section 9.9 Figure 9-9: `P` bit 0, `DM` bit 2, `RH` bit 3,
//!   `TM` bit 4, `DLM` bits 7:5, `IM` bit 15, `V` bits 23:16, `DST` bits 63:32,
//!   `SID` bits 79:64, `SQ` bits 81:80, `SVT` bits 83:82. `DST` is a 32-bit
//!   x2APIC id under `EIME`, and an 8-bit xAPIC id at bits 47:40 without it.
//! - **Remappable MSI address**, Section 5.1.5.2 Figure 5-4 and Table 11:
//!   `FEEh` bits 31:20, `Handle[14:0]` bits 19:5, Interrupt Format bit 4,
//!   `SHV` bit 3, `Handle[15]` bit 2, data `0h`. Section 5.1.3 computes the
//!   entry index as `handle + subhandle`, so `SHV=1` over a zero data word
//!   indexes by the handle alone.
//! - **Remappable I/OxAPIC redirection entry**, Section 5.1.5.1 Figure 5-3:
//!   `Interrupt_Index[14:0]` bits 63:49, Interrupt Format bit 48,
//!   `Interrupt_Index[15]` bit 11, and delivery mode 000b, which is what forces
//!   `SHV` clear in the message the chip then generates.
//!
//! One table serves every unit, which Section 5.1.3 permits explicitly, so an
//! index names the same interrupt whichever unit walks it.
//!
//! `TABLES` is taken before this module's lock and never after it.

use alloc::vec::Vec;

use crate::iommu::StreamId;
use crate::mm::Mmio;
use crate::sync::Lock;

use super::queue::Queue;
use super::table::{Table, Tables};

pub const IRTA_REG: u64 = 0xB8;

pub const INTERRUPT_REMAPPING_ENABLE: u32 = 1 << 25;
pub const SET_TABLE_POINTER: u32 = 1 << 24;
/// Set means compatibility-format messages bypass remapping; `GCMD.CFI` is never written, so it reads clear.
pub const COMPATIBILITY_FORMAT: u32 = 1 << 23;

/// `2^(7+1)` entries of 128 bits is exactly the 4 KiB [`Tables::alloc`] hands out.
const SIZE_FIELD: u64 = 7;
const ENTRIES: u16 = 256;

const EXTENDED_INTERRUPT_MODE: u64 = 1 << 11;

const PRESENT: u64 = 1;
const TRIGGER_LEVEL: u64 = 1 << 4;
const VECTOR_SHIFT: u64 = 16;
const DESTINATION_SHIFT: u64 = 32;
/// `SVT=01b` over `SQ=00b`: a message reaching this entry carries `SID` in all sixteen bits or is refused.
const VERIFY_SOURCE_ID: u64 = 1 << 18;
/// Without `EIME`, `DST` 47:40 holds eight bits and `0xFF` there is broadcast, not a CPU.
const NARROW_DESTINATION_SHIFT: u64 = 8;
const NARROW_DESTINATIONS: u32 = 0xFF;

const MESSAGE_BASE: u32 = 0xFEE0_0000;
const MESSAGE_REMAPPABLE: u32 = 1 << 4;
const MESSAGE_SUBHANDLE_VALID: u32 = 1 << 3;
const PIN_REMAPPABLE: u32 = 1 << 16;

pub struct Msi {
    pub address: u32,
    pub data: u32,
}

pub struct Pin {
    pub low: u32,
    pub high: u32,
}

struct Remap {
    /// `None` until a unit is armed, which is what "no source may use the remappable format" means.
    table: Option<Table>,
    used: u16,
    /// `ECAP.EIM` on every unit. Clear bounds a destination to the eight bits `DST` then holds.
    extended: bool,
    /// Requester ids firmware gave this machine's I/O APICs, which Section 8.3.1.1 requires it to name.
    apics: Vec<(u8, StreamId)>,
    /// Every unit that is remapping, with the queue an entry's write is invalidated through.
    units: Vec<(Mmio, Queue)>,
}

static REMAP: Lock<Remap> =
    Lock::new(Remap { table: None, used: 0, extended: false, apics: Vec::new(), units: Vec::new() });

pub fn describe_apic(apic_id: u8, source: StreamId) {
    REMAP.lock().apics.push((apic_id, source));
}

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

/// Take over a remapping unit's invalidation queue, once `IRE` is confirmed.
///
/// Section 6.4: a unit reporting `CAP.CM`, as these do, may cache the entry a
/// fault was taken on — including a not-present one — so an entry filled in
/// later is invisible until its cache is told, and a stale entry misdelivers.
pub fn adopt(regs: Mmio, queue: Queue) {
    REMAP.lock().units.push((regs, queue));
}

pub fn table_address() -> u64 {
    REMAP.lock().table.map_or(0, Table::phys)
}

pub fn is_armed() -> bool {
    REMAP.lock().table.is_some()
}

/// Fills the next free entry for `source`; `None` refuses rather than
/// mis-delivers — the table is full, or the destination is too wide for `DST`.
fn allocate(source: StreamId, vector: u8, dest: u32, level: bool) -> Option<u16> {
    let index = {
        let mut remap = REMAP.lock();
        let table = remap.table?;
        let too_wide = !remap.extended && dest >= NARROW_DESTINATIONS;
        if too_wide || remap.used == ENTRIES {
            None
        } else {
            let index = remap.used;
            remap.used += 1;
            let destination =
                if remap.extended { dest as u64 } else { (dest as u64) << NARROW_DESTINATION_SHIFT };
            // Delivery mode 000b, destination mode physical and no redirection
            // hint, which is what the compatibility message this replaces said.
            table.write_pair(
                index as usize,
                PRESENT
                    | if level { TRIGGER_LEVEL } else { 0 }
                    | ((vector as u64) << VECTOR_SHIFT)
                    | (destination << DESTINATION_SHIFT),
                VERIFY_SOURCE_ID | source.requester() as u64,
            );
            // Under the same lock as the write, so no other entry can be
            // written between an entry and the invalidation that publishes it.
            for (regs, queue) in &mut remap.units {
                queue.invalidate_interrupts(*regs);
            }
            Some(index)
        }
    };
    match index {
        Some(index) => log!(
            "iommu: irte{index} source={source} sid={:#06x} svt=1 sq=0 vector={vector:#04x} \
             dest={dest} trigger={}",
            source.requester(),
            if level { "level" } else { "edge" }
        ),
        None => log!(
            "iommu: no interrupt remapping entry for {source} at vector {vector:#04x} on apic {dest}"
        ),
    }
    index
}

pub fn msi(source: StreamId, vector: u8, dest: u32) -> Option<Msi> {
    let index = allocate(source, vector, dest, false)? as u32;
    Some(Msi {
        address: MESSAGE_BASE
            | ((index & 0x7FFF) << 5)
            | MESSAGE_REMAPPABLE
            | MESSAGE_SUBHANDLE_VALID
            | ((index >> 15) << 2),
        data: 0,
    })
}

pub fn pin(apic_id: u8, vector: u8, dest: u32, level: bool) -> Option<Pin> {
    let source = apic_source(apic_id)?;
    let index = allocate(source, vector, dest, level)? as u32;
    Some(Pin { low: (index >> 15) << 11, high: PIN_REMAPPABLE | ((index & 0x7FFF) << 17) })
}

/// Locked apart from [`allocate`]: the lock is not reentrant.
fn apic_source(apic_id: u8) -> Option<StreamId> {
    let remap = REMAP.lock();
    remap.apics.iter().find(|(id, _)| *id == apic_id).map(|(_, source)| *source)
}
