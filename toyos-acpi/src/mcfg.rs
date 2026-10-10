//! The MCFG (PCI Firmware Specification 3.3, §4.1.2, Table 4-3): where each
//! PCI segment group's configuration space is, and which of its buses that
//! window decodes.
//!
//! **A window decodes its start bus through its end bus and nothing past
//! either.** Its base is bus 0's address whatever bus it starts at, so a bus
//! outside the range still computes an address — one the platform gave to
//! something else: an interrupt controller, a timer, flash, DRAM, each of which
//! reads as functions that do not exist. What this hands back is the range
//! alongside the base, and every reader indexes only inside it.
//!
//! **One segment group is served**: the first allocation accepted names it,
//! an allocation on another is refused by name, and each bus of the served
//! group is decoded by at most one window.

use core::ops::RangeInclusive;

use crate::{find_table, Phys, Table, TableError, SDT_HEADER_LEN};

/// The first allocation structure, one 8-byte reserved field past the header.
const FIRST_ENTRY: usize = SDT_HEADER_LEN + 8;
/// One allocation structure: base at 0, segment group at 8, start and end bus
/// at 10 and 11, four reserved bytes.
const ENTRY_LEN: usize = 16;
/// Each bus is 32 devices of 8 functions of 4 KiB: one megabyte, so the bus
/// number is address bits 27:20 (PCIe Base 6.0 §7.2.2).
const BUS_SHIFT: u32 = 20;

/// One allocation structure, accepted: its window ends inside the address
/// space and starts on a bus boundary, and its start bus is not past its end.
/// Only [`Allocations`] makes one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Allocation {
    base: u64,
    segment: u16,
    start_bus: u8,
    end_bus: u8,
}

impl Allocation {
    /// Bus 0's configuration space, whatever bus the window starts at.
    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn segment(&self) -> u16 {
        self.segment
    }

    /// The buses the window decodes, never empty.
    pub fn buses(&self) -> RangeInclusive<u8> {
        self.start_bus..=self.end_bus
    }

    /// The bytes the window decodes, as `(address of the start bus, length)`.
    pub fn decoded(&self) -> (u64, u64) {
        let start = self.base + (u64::from(self.start_bus) << BUS_SHIFT);
        let buses = u64::from(self.end_bus - self.start_bus) + 1;
        (start, buses << BUS_SHIFT)
    }

    pub fn holds(&self, bus: u8) -> bool {
        self.buses().contains(&bus)
    }
}

/// Why one allocation structure is not a window this decode hands out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllocationRefused {
    /// The end bus is below the start bus: a window of no bus.
    Inverted { start_bus: u8, end_bus: u8 },
    /// A base off a bus boundary, where no bus number is an address bit.
    Misaligned { base: u64 },
    /// The window's last byte is past the end of the address space.
    Wraps { base: u64, end_bus: u8 },
    /// The table ends inside this structure.
    Partial { bytes: usize },
    /// On a segment group other than the one served.
    OtherSegment { segment: u16, served: u16 },
    /// Buses an earlier window of the served group already decodes.
    Overlaps { start_bus: u8, end_bus: u8 },
}

/// Every allocation structure of an MCFG, in table order.
#[derive(Clone, Copy)]
pub struct Allocations<P> {
    table: Table<P>,
    next: usize,
    served: Option<u16>,
    /// One bit per bus of the served group that a window already decodes.
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
    Ok(Allocations { table, next: FIRST_ENTRY, served: None, claimed: [0; 4] })
}

impl<P: Phys> Iterator for Allocations<P> {
    type Item = Result<Allocation, AllocationRefused>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.next;
        let left = self.table.len.checked_sub(at).filter(|&left| left > 0)?;
        if left < ENTRY_LEN {
            self.next = self.table.len;
            return Some(Err(AllocationRefused::Partial { bytes: left }));
        }
        self.next = at + ENTRY_LEN;
        // Inside the table: `left` covers the whole structure.
        let (base, segment, start_bus, end_bus) = (
            self.table.u64_at(at)?,
            self.table.u16_at(at + 8)?,
            self.table.byte(at + 10)?,
            self.table.byte(at + 11)?,
        );
        Some(self.accept(Allocation { base, segment, start_bus, end_bus }))
    }
}

impl<P> Allocations<P> {
    fn accept(&mut self, window: Allocation) -> Result<Allocation, AllocationRefused> {
        let Allocation { base, segment, start_bus, end_bus } = window;
        if end_bus < start_bus {
            return Err(AllocationRefused::Inverted { start_bus, end_bus });
        }
        if base & ((1 << BUS_SHIFT) - 1) != 0 {
            return Err(AllocationRefused::Misaligned { base });
        }
        // The last byte of the end bus, which `decoded` then sums without a check.
        let last = (u64::from(end_bus) + 1) << BUS_SHIFT;
        if base.checked_add(last - 1).is_none() {
            return Err(AllocationRefused::Wraps { base, end_bus });
        }
        let served = *self.served.get_or_insert(segment);
        if segment != served {
            return Err(AllocationRefused::OtherSegment { segment, served });
        }
        let buses = start_bus..=end_bus;
        if buses.clone().any(|bus| self.claimed[usize::from(bus / 64)] & 1 << (bus % 64) != 0) {
            return Err(AllocationRefused::Overlaps { start_bus, end_bus });
        }
        for bus in buses {
            self.claimed[usize::from(bus / 64)] |= 1 << (bus % 64);
        }
        Ok(window)
    }
}
