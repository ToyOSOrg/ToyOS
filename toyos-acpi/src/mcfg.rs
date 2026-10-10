//! The MCFG (PCI Firmware Specification 3.3, §4.1.2, Table 4-3): where PCI
//! segment group 0's configuration space is, and which of its buses each
//! window decodes.
//!
//! **A window decodes its start bus through its end bus and nothing past
//! either.** Its base is bus 0's address whatever bus it starts at, so a bus
//! outside the range still computes an address — one the platform gave to
//! something else: an interrupt controller, a timer, flash, DRAM, each of which
//! reads as functions that do not exist. [`EcamWindow`] is the only reader of
//! that arithmetic, and it answers only inside the range.
//!
//! **Segment group 0 alone is served** ([`SEGMENT_GROUP`]): it is the one every
//! machine has, and a window on another is refused by name. Each of its buses
//! is decoded by at most one window.

use core::ops::RangeInclusive;

use toyos_abi::boot::MemoryMapEntry;

use crate::{find_table, Phys, Table, TableError, SDT_HEADER_LEN};

/// The first allocation structure, one 8-byte reserved field past the header.
const FIRST_ENTRY: usize = SDT_HEADER_LEN + 8;
/// One allocation structure: base at 0, segment group at 8, start and end bus
/// at 10 and 11, four reserved bytes.
const ENTRY_LEN: usize = 16;
/// Each bus is 32 devices of 8 functions of 4 KiB: one megabyte, so the bus
/// number is address bits 27:20 (PCIe Base 6.0 §7.2.2).
const BUS_SHIFT: u32 = 20;
const DEVICE_SHIFT: u32 = 15;
const FUNCTION_SHIFT: u32 = 12;
/// One function's configuration space.
const FUNCTION_BYTES: u64 = 1 << FUNCTION_SHIFT;

/// The PCI segment group every window this decode accepts is on.
pub const SEGMENT_GROUP: u16 = 0;

/// One window of segment group 0's configuration space, as an allocation
/// structure names it: it starts on a bus boundary, its start bus is not past
/// its end, and its last byte is inside the address space. Only
/// [`EcamWindow::decode`] makes one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EcamWindow {
    base: u64,
    start_bus: u8,
    end_bus: u8,
}

/// A function's configuration register, as an address in a window names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfigRegister {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub offset: u16,
}

impl EcamWindow {
    /// One allocation structure's bytes, accepted or refused by name.
    pub fn decode(entry: &[u8; ENTRY_LEN]) -> Result<Self, AllocationRefused> {
        let [b0, b1, b2, b3, b4, b5, b6, b7, s0, s1, start_bus, end_bus, ..] = *entry;
        let base = u64::from_le_bytes([b0, b1, b2, b3, b4, b5, b6, b7]);
        let segment = u16::from_le_bytes([s0, s1]);
        if segment != SEGMENT_GROUP {
            return Err(AllocationRefused::OtherSegment { segment });
        }
        if end_bus < start_bus {
            return Err(AllocationRefused::Inverted { start_bus, end_bus });
        }
        if base & ((1 << BUS_SHIFT) - 1) != 0 {
            return Err(AllocationRefused::Misaligned { base });
        }
        // The last byte of the end bus: what every other method then sums
        // without a check.
        if base.checked_add(((u64::from(end_bus) + 1) << BUS_SHIFT) - 1).is_none() {
            return Err(AllocationRefused::Wraps { base, end_bus });
        }
        Ok(Self { base, start_bus, end_bus })
    }

    /// Bus 0's configuration space, whatever bus the window starts at.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// The buses the window decodes, never empty.
    pub fn buses(&self) -> RangeInclusive<u8> {
        self.start_bus..=self.end_bus
    }

    pub fn holds(&self, bus: u8) -> bool {
        self.buses().contains(&bus)
    }

    /// The bytes the window decodes, as `(address of the start bus, length)`.
    pub fn decoded(&self) -> (u64, u64) {
        let start = self.base + (u64::from(self.start_bus) << BUS_SHIFT);
        let buses = u64::from(self.end_bus - self.start_bus) + 1;
        (start, buses << BUS_SHIFT)
    }

    /// Where a function's configuration space is, from the window's first
    /// byte ([`Self::decoded`]'s start); `None` for a bus the window does not
    /// decode, or a device or function no address names.
    pub fn offset(&self, bus: u8, device: u8, function: u8) -> Option<u64> {
        if !self.holds(bus) || device > 31 || function > 7 {
            return None;
        }
        Some(
            u64::from(bus - self.start_bus) << BUS_SHIFT
                | u64::from(device) << DEVICE_SHIFT
                | u64::from(function) << FUNCTION_SHIFT,
        )
    }

    /// The register an address names, if the window decodes it.
    pub fn locate(&self, at: u64) -> Option<ConfigRegister> {
        let (start, bytes) = self.decoded();
        let into = at.checked_sub(start).filter(|&into| into < bytes)?;
        Some(ConfigRegister {
            bus: self.start_bus + (into >> BUS_SHIFT) as u8,
            device: (into >> DEVICE_SHIFT & 0x1F) as u8,
            function: (into >> FUNCTION_SHIFT & 7) as u8,
            offset: (into % FUNCTION_BYTES) as u16,
        })
    }

    /// Whether the kernel can map exactly this window, uncached: on its
    /// mapping's grain, below `limit`, and over no memory firmware's `map`
    /// lists as memory, which the kernel already maps cached.
    pub fn mappable(&self, grain: u64, limit: u64, map: &[MemoryMapEntry]) -> Result<(), AllocationRefused> {
        let (start, bytes) = self.decoded();
        if start % grain != 0 || bytes % grain != 0 {
            return Err(AllocationRefused::OffGrain { start, bytes, grain });
        }
        // At most `u64::MAX`: `decode` bounded the last byte.
        let last = start + (bytes - 1);
        if last >= limit {
            return Err(AllocationRefused::PastLimit { last, limit });
        }
        match map.iter().find(|entry| {
            toyos_bootmap::is_read_as_memory(entry.uefi_type) && entry.start <= last && start < entry.end
        }) {
            Some(entry) => Err(AllocationRefused::OverMemory { uefi_type: entry.uefi_type, start: entry.start }),
            None => Ok(()),
        }
    }
}

/// Why one allocation structure is not a window this kernel uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllocationRefused {
    /// On a segment group other than [`SEGMENT_GROUP`].
    OtherSegment { segment: u16 },
    /// The end bus is below the start bus: a window of no bus.
    Inverted { start_bus: u8, end_bus: u8 },
    /// A base off a bus boundary, where no bus number is an address bit.
    Misaligned { base: u64 },
    /// The window's last byte is past the end of the address space.
    Wraps { base: u64, end_bus: u8 },
    /// The table ends inside this structure.
    Partial { bytes: usize },
    /// Buses an earlier window already decodes.
    Overlaps { start_bus: u8, end_bus: u8 },
    /// Its bytes start or end off the grain the kernel maps at, so a mapping
    /// of it would take the neighbouring bytes too.
    OffGrain { start: u64, bytes: u64, grain: u64 },
    /// Its last byte is at or past the first address the kernel can map.
    PastLimit { last: u64, limit: u64 },
    /// Firmware's map lists memory, of this type and from this address, in it.
    OverMemory { uefi_type: u32, start: u64 },
}

/// Every allocation structure of an MCFG, in table order.
#[derive(Clone, Copy)]
pub struct Allocations<P> {
    table: Table<P>,
    next: usize,
    /// One bit per bus a window already decodes.
    claimed: [u64; 4],
}

impl<P> Allocations<P> {
    /// Where firmware put the MCFG.
    pub fn table_base(&self) -> u64 {
        self.table.base
    }
}

/// The MCFG at `rsdp_addr`, opened for its first allocation structure.
pub fn ecam_allocations<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Allocations<P>, TableError> {
    let table = find_table(phys, rsdp_addr, b"MCFG", FIRST_ENTRY + ENTRY_LEN)?;
    Ok(Allocations { table, next: FIRST_ENTRY, claimed: [0; 4] })
}

impl<P: Phys> Iterator for Allocations<P> {
    type Item = Result<EcamWindow, AllocationRefused>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.next;
        let left = self.table.len.checked_sub(at).filter(|&left| left > 0)?;
        if left < ENTRY_LEN {
            self.next = self.table.len;
            return Some(Err(AllocationRefused::Partial { bytes: left }));
        }
        self.next = at + ENTRY_LEN;
        let mut entry = [0u8; ENTRY_LEN];
        for (i, byte) in entry.iter_mut().enumerate() {
            // Inside the table: `left` covers the whole structure.
            *byte = self.table.byte(at + i)?;
        }
        Some(EcamWindow::decode(&entry).and_then(|window| self.claim(window)))
    }
}

impl<P> Allocations<P> {
    fn claim(&mut self, window: EcamWindow) -> Result<EcamWindow, AllocationRefused> {
        let claimed = |bus: u8| self.claimed[usize::from(bus / 64)] & 1 << (bus % 64) != 0;
        if window.buses().any(claimed) {
            return Err(AllocationRefused::Overlaps { start_bus: window.start_bus, end_bus: window.end_bus });
        }
        for bus in window.buses() {
            self.claimed[usize::from(bus / 64)] |= 1 << (bus % 64);
        }
        Ok(window)
    }
}
