//! The `DMAR` table: what firmware says about this machine's remapping units.
//!
//! Firmware input: a malformed entry is refused, never panicked, and every
//! read is bounded by the length `find_table` validated and checksummed. A
//! zero-length entry, or one past its list, ends the walk at every level.
//!
//! Nothing here allocates or is stored; a [`Dmar`] is an offset into the
//! validated table, held where firmware left it.

use core::mem::size_of;

use crate::drivers::acpi::{find_table, Table, TableError};
use crate::iommu::StreamId;

/// Header (36) + Host Address Width (1) + Flags (1) + 10 reserved; first structure starts here.
const REMAPPING_STRUCTURES: usize = 48;

const HOST_ADDRESS_WIDTH: usize = 36;
const FLAGS: usize = 37;

/// Flags bit 0: unset means the platform cannot remap interrupts.
pub const FLAG_INTR_REMAP: u8 = 1 << 0;
/// Flags bit 1: firmware asks the OS not to enable x2APIC mode.
pub const FLAG_X2APIC_OPT_OUT: u8 = 1 << 1;
/// Flags bit 2: DMA blocked until the OS programs the unit; honoured for free by this kernel's ordering.
pub const FLAG_DMA_CTRL_OPT_IN: u8 = 1 << 2;

/// A list element whose declared size the list cannot hold; the last item a walk yields.
pub struct Malformed {
    pub at: usize,
    pub declared: usize,
}

/// A validated `DMAR` table's two header fields, read once since the whole table's length already bounds them.
#[derive(Clone, Copy)]
pub struct Dmar {
    table: Table,
    /// The widest physical address the units can produce; firmware reports it one less.
    pub host_address_width: u16,
    pub flags: u8,
}

impl Dmar {
    pub fn open(rsdp_addr: u64) -> Result<Self, TableError> {
        let table = find_table(rsdp_addr, b"DMAR", REMAPPING_STRUCTURES)?;
        let length = || TableError::Length {
            declared: table.len() as u32,
            needed: REMAPPING_STRUCTURES,
        };
        let haw: u8 = table.field(HOST_ADDRESS_WIDTH).ok_or_else(length)?;
        let flags: u8 = table.field(FLAGS).ok_or_else(length)?;
        Ok(Self { table, host_address_width: haw as u16 + 1, flags })
    }

    pub fn structures(&self) -> Structures {
        Structures { table: self.table, offset: REMAPPING_STRUCTURES }
    }
}

/// One remapping structure.
pub enum Structure {
    Drhd(Drhd),
    Rmrr(Rmrr),
    /// A type this kernel walks past, carried so the skip can be logged with its kind.
    Skipped { kind: u16, at: usize, len: usize },
}

pub struct Structures {
    table: Table,
    offset: usize,
}

/// Every type this kernel knows the name of; an unknown type is still logged, by number.
pub fn structure_name(kind: u16) -> &'static str {
    match kind {
        0 => "DRHD",
        1 => "RMRR",
        2 => "ATSR",
        3 => "RHSA",
        4 => "ANDD",
        5 => "SATC",
        6 => "SIDP",
        _ => "unknown",
    }
}

impl Iterator for Structures {
    type Item = Result<Structure, Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.offset;
        let kind: u16 = self.table.field(at)?;
        let declared: u16 = self.table.field(at + 2)?;
        let len = declared as usize;
        // A list that lied about an element's size cannot be resynchronised.
        if len < 4 || at.checked_add(len)? > self.table.len() {
            // Ends the walk: offset lands past the table so the next call yields nothing.
            self.offset = self.table.len();
            return Some(Err(Malformed { at, declared: len }));
        }
        self.offset = at + len;
        Some(Ok(match kind {
            0 => Structure::Drhd(Drhd { table: self.table, at, len }),
            1 => Structure::Rmrr(Rmrr { table: self.table, at, len }),
            kind => Structure::Skipped { kind, at, len },
        }))
    }
}

/// Bounded by the structure's own declared length, not the table's — past it, the next structure's bytes would be misread as this field.
fn bounded<T: Copy + Default>(table: Table, at: usize, len: usize, offset: usize) -> T {
    match offset.checked_add(size_of::<T>()) {
        Some(end) if end <= len => table.field(at + offset).unwrap_or_default(),
        _ => T::default(),
    }
}

/// Type 0: one hardware unit's register window and the devices in its scope.
#[derive(Clone, Copy)]
pub struct Drhd {
    table: Table,
    at: usize,
    len: usize,
}

/// DRHD flags bit 0: this unit is the catch-all for everything on its segment that no other unit's scope names.
const DRHD_INCLUDE_PCI_ALL: u8 = 1 << 0;

const DRHD_FLAGS: usize = 4;
const DRHD_SEGMENT: usize = 6;
const DRHD_REGISTER_BASE: usize = 8;
const DRHD_SCOPES: usize = 16;

impl Drhd {
    pub fn include_pci_all(&self) -> bool {
        self.field::<u8>(DRHD_FLAGS) & DRHD_INCLUDE_PCI_ALL != 0
    }

    pub fn segment(&self) -> u16 {
        self.field(DRHD_SEGMENT)
    }

    /// Where the unit's 4 KiB register window is; the caller must validate before mapping it.
    pub fn register_base(&self) -> u64 {
        self.field(DRHD_REGISTER_BASE)
    }

    pub fn scopes(&self) -> Scopes {
        Scopes { table: self.table, offset: self.at + DRHD_SCOPES, end: self.at + self.len }
    }

    fn field<T: Copy + Default>(&self, offset: usize) -> T {
        bounded(self.table, self.at, self.len, offset)
    }
}

/// Type 1: a physical range firmware requires stay identity-mapped for the devices in its scope.
#[derive(Clone, Copy)]
pub struct Rmrr {
    table: Table,
    at: usize,
    len: usize,
}

const RMRR_SEGMENT: usize = 6;
const RMRR_BASE: usize = 8;
const RMRR_LIMIT: usize = 16;
const RMRR_SCOPES: usize = 24;

impl Rmrr {
    pub fn segment(&self) -> u16 {
        self.field(RMRR_SEGMENT)
    }

    pub fn base(&self) -> u64 {
        self.field(RMRR_BASE)
    }

    /// Inclusive, as firmware states it.
    pub fn limit(&self) -> u64 {
        self.field(RMRR_LIMIT)
    }

    pub fn scopes(&self) -> Scopes {
        Scopes { table: self.table, offset: self.at + RMRR_SCOPES, end: self.at + self.len }
    }

    fn field<T: Copy + Default>(&self, offset: usize) -> T {
        bounded(self.table, self.at, self.len, offset)
    }
}

/// A device scope list, bounded by its parent structure's declared length.
pub struct Scopes {
    table: Table,
    offset: usize,
    end: usize,
}

pub struct Scope {
    table: Table,
    at: usize,
    len: usize,
}

const SCOPE_ENUMERATION_ID: usize = 4;
const SCOPE_START_BUS: usize = 5;
/// Type (1) + Length (1) + 2 reserved + Enumeration ID (1) + Start Bus (1); path starts here.
const SCOPE_PATH: usize = 6;

impl Iterator for Scopes {
    type Item = Result<Scope, Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.offset;
        if at + SCOPE_PATH > self.end {
            return None;
        }
        let len = self.table.field::<u8>(at + 1)? as usize;
        if len < SCOPE_PATH || at + len > self.end {
            self.offset = self.end;
            return Some(Err(Malformed { at, declared: len }));
        }
        self.offset = at + len;
        Some(Ok(Scope { table: self.table, at, len }))
    }
}

impl Scope {
    /// 1 PCI endpoint, 2 PCI sub-hierarchy, 3 I/O APIC, 4 MSI-capable HPET, 5 ACPI namespace device.
    pub fn kind(&self) -> u8 {
        self.field(0)
    }

    pub fn kind_name(&self) -> &'static str {
        match self.kind() {
            1 => "pci-endpoint",
            2 => "pci-bridge",
            3 => "ioapic",
            4 => "hpet",
            5 => "acpi-device",
            _ => "unknown",
        }
    }

    /// The I/O APIC id or HPET number this scope names, for the two types that carry one.
    pub fn enumeration_id(&self) -> u8 {
        self.field(SCOPE_ENUMERATION_ID)
    }

    pub fn start_bus(&self) -> u8 {
        self.field(SCOPE_START_BUS)
    }

    /// `(device, function)` from the start bus down to the named device.
    pub fn path(&self) -> impl Iterator<Item = (u8, u8)> + '_ {
        let entries = (self.len - SCOPE_PATH) / 2;
        (0..entries).map(move |i| {
            let at = SCOPE_PATH + i * 2;
            (self.field(at), self.field(at + 1))
        })
    }

    /// The requester id this scope names directly; `None` through a bridge,
    /// whose secondary bus is not in this table and cannot be guessed.
    pub fn stream_id(&self) -> Option<StreamId> {
        let mut path = self.path();
        let (device, function) = path.next()?;
        path.next().is_none().then(|| StreamId::pci(self.start_bus(), device, function))
    }

    fn field<T: Copy + Default>(&self, offset: usize) -> T {
        bounded(self.table, self.at, self.len, offset)
    }
}
