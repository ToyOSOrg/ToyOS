//! The MADT (signature `APIC`): ACPI 6.5 §5.2.12, Table 5.19.
//!
//! The list is walked by each entry's own declared length, so a zero length or
//! one running past the list ends the walk with a [`MadtHalt`]: neither can be
//! resynchronised, and a walk that tried would not terminate.

use crate::{u16le, u32le, Phys, Table, SDT_HEADER_LEN};

/// ACPI 6.5 Table 5.19: `Local Interrupt Controller Address` (4) and `Flags`
/// (4) follow the header; the interrupt controller structures start here.
pub const MADT_ENTRIES: usize = SDT_HEADER_LEN + 8;

/// ACPI 6.5 Table 5.20: every structure begins with a type byte and a length byte.
const ENTRY_HEADER_LEN: usize = 2;

/// MADT type 1: an I/O APIC's register window and its first GSI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IoApicEntry {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

/// MADT type 2: an ISA IRQ override.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceOverride {
    pub bus: u8,
    pub source_irq: u8,
    pub gsi: u32,
    /// The raw MPS INTI word (bits 0-1 polarity, 2-3 trigger).
    pub flags: u16,
}

/// One interrupt controller structure, in the shape the caller acts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MadtEntry {
    /// Type 0 (Table 5.21) or type 9 (Table 5.27); `enabled` is flags bit 0.
    LocalApic { apic_id: u32, enabled: bool },
    IoApic(IoApicEntry),
    SourceOverride(SourceOverride),
    /// A type this kernel does not act on, or one too short to hold its own fields.
    Other(u8),
}

/// The entry whose declared length the list cannot hold; the last item a walk yields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MadtHalt {
    /// Byte offset within the structure list, as the caller's log line states it.
    pub at: usize,
    pub declared: usize,
    pub list_len: usize,
}

/// Every interrupt controller structure, in firmware's order.
pub struct MadtEntries<P> {
    table: Table<P>,
    offset: usize,
    list_len: usize,
    halted: bool,
}

/// Walk a validated MADT. The table must have been opened for at least
/// [`MADT_ENTRIES`] bytes, which is what makes the list length total.
pub fn madt_entries<P: Phys>(madt: &Table<P>) -> MadtEntries<P> {
    MadtEntries {
        table: *madt,
        offset: 0,
        list_len: madt.len().saturating_sub(MADT_ENTRIES),
        halted: false,
    }
}

impl<P: Phys> Iterator for MadtEntries<P> {
    type Item = Result<MadtEntry, MadtHalt>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.halted || self.offset + ENTRY_HEADER_LEN > self.list_len {
            return None;
        }
        let at = MADT_ENTRIES + self.offset;
        let entry_type = self.table.byte(at)?;
        let declared = self.table.byte(at + 1)? as usize;
        // A zero-length or overrunning entry can't be resynchronised, so both end the walk.
        if declared < ENTRY_HEADER_LEN || self.offset + declared > self.list_len {
            self.halted = true;
            return Some(Err(MadtHalt {
                at: self.offset,
                declared,
                list_len: self.list_len,
            }));
        }
        self.offset += declared;

        let phys = self.table.phys();
        let base = self.table.base() + at as u64;
        Some(Ok(match (entry_type, declared) {
            // Table 5.21: ACPI Processor UID (2), APIC ID (3), Flags (4..8).
            (0, 8..) => MadtEntry::LocalApic {
                apic_id: u32::from(phys.byte(base + 3)),
                enabled: u32le(phys, base + 4) & 1 != 0,
            },
            // Table 5.22: I/O APIC ID (2), Address (4..8), GSI Base (8..12).
            (1, 12..) => MadtEntry::IoApic(IoApicEntry {
                id: phys.byte(base + 2),
                address: u32le(phys, base + 4),
                gsi_base: u32le(phys, base + 8),
            }),
            // Table 5.23: Bus (2), Source (3), GSI (4..8), MPS INTI Flags (8..10).
            (2, 10..) => MadtEntry::SourceOverride(SourceOverride {
                bus: phys.byte(base + 2),
                source_irq: phys.byte(base + 3),
                gsi: u32le(phys, base + 4),
                flags: u16le(phys, base + 8),
            }),
            // Table 5.27: X2APIC ID (4..8), Flags (8..12).
            (9, 16..) => MadtEntry::LocalApic {
                apic_id: u32le(phys, base + 4),
                enabled: u32le(phys, base + 8) & 1 != 0,
            },
            _ => MadtEntry::Other(entry_type),
        }))
    }
}
