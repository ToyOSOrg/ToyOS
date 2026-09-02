//! I/O APIC — the only path a pin interrupt has into this kernel; every
//! other device is MSI-X.
//!
//! `init` must run between `lidt` and the first `sti`: an unmasked entry left
//! by firmware that fires before then hits an unhandled vector and panics
//! the boot. Register access is index-write then data-read, never atomic:
//! `TOPOLOGY` serializes it and is taken from thread context only, never
//! from an ISR.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::iommu::Delivery;
use crate::mm::paging::MmioPolicy;
use crate::log;
use crate::mm::Mmio;
use crate::sync::Lock;
use super::acpi::MadtInfo;

const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;

const REG_VER: u32 = 0x01;
const REG_REDTBL: u32 = 0x10;

const RTE_DELIVERY_STATUS: u32 = 1 << 12;
const RTE_POLARITY_LOW: u32 = 1 << 13;
const RTE_REMOTE_IRR: u32 = 1 << 14;
const RTE_TRIGGER_LEVEL: u32 = 1 << 15;
const RTE_MASKED: u32 = 1 << 16;

// 8 bits of "max redirection entry" in hardware; no shipped part is near it.
const MAX_PLAUSIBLE_ENTRIES: u32 = 240;

/// Global System Interrupt: the flat interrupt-input space the MADT numbers I/O APIC pins in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gsi(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Edge,
    Level,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Polarity {
    High,
    Low,
}

/// An ISA line resolved against the override table: GSI plus trigger and polarity.
#[derive(Clone, Copy, Debug)]
pub struct IsaLine {
    pub gsi: Gsi,
    pub trigger: Trigger,
    pub polarity: Polarity,
}

pub enum RouteError {
    /// No discovered unit covers this GSI.
    /// Callers must refuse the device, not assume the pin works.
    NoUnit(Gsi),
    /// Destination APIC id does not fit the 8-bit field (0xFF is broadcast).
    DestTooWide(u32),
    /// The IOMMU remaps interrupts and had no entry to give this pin.
    NotRemappable(Gsi),
    /// The written redirection entry did not read back unchanged.
    Readback { wrote: u64, read: u64 },
}

/// Hand-written: `derive(Debug)` would print register values in decimal.
impl core::fmt::Debug for RouteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoUnit(gsi) => write!(f, "no I/O APIC covers GSI {}", gsi.0),
            Self::DestTooWide(id) => write!(f, "apic id {id:#x} does not fit an 8-bit destination"),
            Self::NotRemappable(gsi) => {
                write!(f, "the IOMMU has no remapping entry for GSI {}", gsi.0)
            }
            Self::Readback { wrote, read } => {
                write!(f, "wrote {wrote:#018x}, read back {read:#018x}")
            }
        }
    }
}

struct Unit {
    mmio: Mmio,
    /// The MADT's id for this chip, which is also the name a DMAR device scope gives its source id.
    id: u8,
    gsi_base: u32,
    entries: u32,
}

impl Unit {
    fn read(&self, index: u32) -> u32 {
        self.mmio.write_u32(IOREGSEL, index);
        self.mmio.read_u32(IOWIN)
    }

    fn write(&self, index: u32, value: u32) {
        self.mmio.write_u32(IOREGSEL, index);
        self.mmio.write_u32(IOWIN, value);
    }
}

struct Override {
    source_irq: u8,
    gsi: u32,
    trigger: Trigger,
    polarity: Polarity,
}

struct Topology {
    units: Vec<Unit>,
    overrides: Vec<Override>,
}

static TOPOLOGY: Lock<Topology> = Lock::new(Topology {
    units: Vec::new(),
    overrides: Vec::new(),
});

pub fn init(madt: &MadtInfo) {
    let mut topology = TOPOLOGY.lock();

    for entry in &madt.io_apics {
        // 0x20 covers IOREGSEL and IOWIN; every entry is reached through those two.
        let mmio = crate::mm::paging::map_mmio(entry.address as u64, 0x20, MmioPolicy::Uncacheable);
        let mut unit = Unit { mmio, id: entry.id, gsi_base: entry.gsi_base, entries: 0 };
        let ver = unit.read(REG_VER);
        let version = ver & 0xFF;
        unit.entries = ((ver >> 16) & 0xFF) + 1;
        // version and entries both come from REG_VER: 0x00/0xFF is what undecoded MMIO returns, not a real chip.
        if version == 0x00
            || version == 0xFF
            || unit.entries > MAX_PLAUSIBLE_ENTRIES
        {
            // An undecoded window would claim every GSI and route into the void.
            log!(
                "ioapic: id={} at {:#x} IGNORED — version register {:#010x} is not a redirection table",
                entry.id,
                entry.address,
                ver
            );
            continue;
        }
        // Masks every entry, including any that would carry the ACPI SCI: no power-button or lid events.
        let mut masked = 0;
        for n in 0..unit.entries {
            unit.write(REG_REDTBL + 2 * n, RTE_MASKED);
            // Read back rather than trust the write: an unmasked entry is the hazard this loop exists to prevent.
            if unit.read(REG_REDTBL + 2 * n) & RTE_MASKED != 0 {
                masked += 1;
            }
        }
        log!(
            "ioapic: id={} at {:#x} ver={:#04x} gsi {}..{} masked {}/{}",
            entry.id,
            entry.address,
            version,
            unit.gsi_base,
            unit.gsi_base + unit.entries - 1,
            masked,
            unit.entries
        );
        topology.units.push(unit);
    }

    // One line for the whole table: the no-UART log tail holds a fixed number of rows.
    let mut table = String::new();
    for iso in &madt.source_overrides {
        // MPS INTI flags: 00 means "conforms to bus" — ISA default is edge/high.
        let polarity = match iso.flags & 0x3 {
            3 => Polarity::Low,
            _ => Polarity::High,
        };
        let trigger = match (iso.flags >> 2) & 0x3 {
            3 => Trigger::Level,
            _ => Trigger::Edge,
        };
        let _ = write!(
            table,
            "{}{}:{}->{} {}",
            if table.is_empty() { "" } else { ", " },
            iso.bus,
            iso.source_irq,
            iso.gsi,
            describe(trigger, polarity)
        );
        topology.overrides.push(Override {
            source_irq: iso.source_irq,
            gsi: iso.gsi,
            trigger,
            polarity,
        });
    }
    log!("ioapic: iso bus:irq->gsi [{}]", table);

    if topology.units.is_empty() {
        log!("ioapic: none in MADT — no pin interrupts on this machine");
    }
}

fn describe(trigger: Trigger, polarity: Polarity) -> &'static str {
    match (trigger, polarity) {
        (Trigger::Edge, Polarity::High) => "edge/high",
        (Trigger::Edge, Polarity::Low) => "edge/low",
        (Trigger::Level, Polarity::High) => "level/high",
        (Trigger::Level, Polarity::Low) => "level/low",
    }
}

/// Where ISA `irq` lands and how it is driven, or `None` when no I/O APIC exists.
pub fn gsi_for_isa_irq(irq: u8) -> Option<IsaLine> {
    let topology = TOPOLOGY.lock();
    if topology.units.is_empty() {
        return None;
    }
    Some(
        topology
            .overrides
            .iter()
            .find(|o| o.source_irq == irq)
            .map_or(
                IsaLine {
                    gsi: Gsi(irq as u32),
                    trigger: Trigger::Edge,
                    polarity: Polarity::High,
                },
                |o| IsaLine { gsi: Gsi(o.gsi), trigger: o.trigger, polarity: o.polarity },
            ),
    )
}

/// Every chip this machine routes pins through, by MADT id: the IOMMU needs the
/// whole set before it can decide anything, and decides once for the machine.
pub fn ids() -> Vec<u8> {
    TOPOLOGY.lock().units.iter().map(|u| u.id).collect()
}

fn locate(topology: &Topology, gsi: Gsi) -> Result<(&Unit, u32), RouteError> {
    topology
        .units
        .iter()
        .find(|u| gsi.0 >= u.gsi_base && gsi.0 < u.gsi_base + u.entries)
        .map(|u| (u, gsi.0 - u.gsi_base))
        .ok_or(RouteError::NoUnit(gsi))
}

/// Point `gsi` at `vector` on one CPU, fixed delivery, physical destination; the entry is left masked.
///
/// Under remapping the entry names a table slot and the destination lives in
/// that slot — but the id must still fit whatever names it, so `DestTooWide`
/// moves rather than disappearing and arrives as [`Delivery::Refused`].
pub fn route(
    gsi: Gsi,
    vector: u8,
    dest_apic_id: u32,
    trigger: Trigger,
    polarity: Polarity,
) -> Result<(), RouteError> {
    let topology = TOPOLOGY.lock();
    let (unit, n) = locate(&topology, gsi)?;
    let level = trigger == Trigger::Level;
    let (index, high) =
        match crate::iommu::remap_pin(unit.id, vector, dest_apic_id, level) {
            Delivery::Direct => {
                if dest_apic_id >= 0xFF {
                    return Err(RouteError::DestTooWide(dest_apic_id));
                }
                (0, dest_apic_id << 24)
            }
            Delivery::Remapped(pin) => (pin.low, pin.high),
            Delivery::Refused => return Err(RouteError::NotRemappable(gsi)),
        };
    let low = vector as u32
        | index
        | RTE_MASKED
        | if polarity == Polarity::Low { RTE_POLARITY_LOW } else { 0 }
        | if level { RTE_TRIGGER_LEVEL } else { 0 };
    // Destination first: writing the low word last means it is never briefly armed at the old destination.
    unit.write(REG_REDTBL + 2 * n + 1, high);
    unit.write(REG_REDTBL + 2 * n, low);
    // Delivery status (12) and remote IRR (14) are the chip's, not ours.
    let read_low = unit.read(REG_REDTBL + 2 * n) & !(RTE_DELIVERY_STATUS | RTE_REMOTE_IRR);
    let read_high = unit.read(REG_REDTBL + 2 * n + 1);
    if read_low != low || read_high != high {
        return Err(RouteError::Readback {
            wrote: u64::from(high) << 32 | u64::from(low),
            read: u64::from(read_high) << 32 | u64::from(read_low),
        });
    }
    // The entry as the chip holds it, which is the only evidence of what format
    // a pin is really in — what the kernel meant to write is not the same claim.
    log!(
        "ioapic: gsi {} on id={} rte={:#018x}",
        gsi.0,
        unit.id,
        u64::from(read_high) << 32 | u64::from(read_low)
    );
    Ok(())
}

/// The redirection entry for `gsi` exactly as the chip holds it, high word first, or `None` when no unit covers it or the topology is busy.
/// Raw, not decoded: a decode would have to guess which field the caller needs, and this exists to catch an unexpected one.
#[cfg(feature = "boot-actuators")]
pub fn redirection(gsi: Gsi) -> Option<u64> {
    // try_lock: the caller runs in the idle loop on a possibly-stopped machine.
    let topology = TOPOLOGY.try_lock()?;
    let (unit, n) = locate(&topology, gsi).ok()?;
    let low = unit.read(REG_REDTBL + 2 * n);
    let high = unit.read(REG_REDTBL + 2 * n + 1);
    Some(u64::from(high) << 32 | u64::from(low))
}

pub fn set_masked(gsi: Gsi, masked: bool) -> Result<(), RouteError> {
    let topology = TOPOLOGY.lock();
    let (unit, n) = locate(&topology, gsi)?;
    let index = REG_REDTBL + 2 * n;
    let low = unit.read(index);
    unit.write(index, if masked { low | RTE_MASKED } else { low & !RTE_MASKED });
    Ok(())
}
