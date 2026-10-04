//! I/O APIC — the only path a pin interrupt has into this kernel; every
//! other device is MSI-X.
//!
//! `init` must run between `lidt` and the first `sti`: an unmasked entry left
//! by firmware that fires before then hits an unhandled vector and panics
//! the boot. The topology is written once there, on the BSP, and only read
//! after. A unit's registers are an index write then a data access, never
//! atomic: each unit's pair is its own [`Masked`] lock, so an interrupt
//! handler masks its own line through it. A routed pin keeps the low word of
//! its entry, so a mask or an unmask is one write of that word and reads
//! nothing back.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use toyos_acpi::SourceOverride;
pub use toyos_acpi::{Polarity, Trigger};

use crate::arch::IrqGuard;
use crate::drivers::acpi::MadtInfo;
use crate::iommu::Delivery;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;
use crate::sync::Masked;

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

/// A line resolved against the override table: GSI plus trigger and polarity.
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
    /// The IOMMU remaps interrupts and had no entry to give this pin, and why.
    NotRemappable(Gsi, crate::iommu::Refused),
    /// The written redirection entry did not read back unchanged.
    Readback { wrote: u64, read: u64 },
}

/// Hand-written: `derive(Debug)` would print register values in decimal.
impl core::fmt::Debug for RouteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoUnit(gsi) => write!(f, "no I/O APIC covers GSI {}", gsi.0),
            Self::DestTooWide(id) => write!(f, "apic id {id:#x} does not fit an 8-bit destination"),
            Self::NotRemappable(gsi, why) => write!(f, "GSI {} cannot be remapped: {why}", gsi.0),
            Self::Readback { wrote, read } => {
                write!(f, "wrote {wrote:#018x}, read back {read:#018x}")
            }
        }
    }
}

struct Unit {
    /// The MADT's id for this chip, which is also the name a DMAR device scope gives its source id.
    id: u8,
    gsi_base: u32,
    entries: u32,
    registers: Masked<Window>,
    /// Each pin's entry's low word as `route` wrote it, unmasked; 0 for a
    /// pin never routed.
    lows: Box<[AtomicU32]>,
}

/// A unit's index/data pair, reached only through its lock.
struct Window(Mmio);

impl Window {
    fn read(&self, index: u32) -> u32 {
        self.0.write_u32(IOREGSEL, index);
        self.0.read_u32(IOWIN)
    }

    fn write(&self, index: u32, value: u32) {
        self.0.write_u32(IOREGSEL, index);
        self.0.write_u32(IOWIN, value);
    }
}

struct Topology {
    units: Vec<Unit>,
    overrides: Vec<SourceOverride>,
}

/// Written once, by [`init`].
static TOPOLOGY: AtomicPtr<Topology> = AtomicPtr::new(core::ptr::null_mut());

fn topology() -> &'static Topology {
    let at = TOPOLOGY.load(Ordering::Acquire);
    assert!(!at.is_null(), "ioapic: read before init");
    // SAFETY: `init` stored a leaked `Box` exactly once and nothing frees it.
    unsafe { &*at }
}

pub fn init(madt: &MadtInfo) {
    let mut units = Vec::new();
    for entry in &madt.io_apics {
        // 0x20 covers IOREGSEL and IOWIN; every entry is reached through those two.
        let mmio = crate::mm::paging::map_mmio(entry.address as u64, 0x20, MmioPolicy::Uncacheable);
        let mut unit = Unit {
            id: entry.id,
            gsi_base: entry.gsi_base,
            entries: 0,
            registers: Masked::new(Window(mmio)),
            lows: Box::new([]),
        };
        let irq = IrqGuard::close();
        let pair = unit.registers.lock(&irq);
        let ver = pair.read(REG_VER);
        let version = ver & 0xFF;
        let entries = ((ver >> 16) & 0xFF) + 1;
        // version and entries both come from REG_VER: 0x00/0xFF is what undecoded MMIO returns, not a real chip.
        if version == 0x00 || version == 0xFF || entries > MAX_PLAUSIBLE_ENTRIES {
            drop(pair);
            // An undecoded window would claim every GSI and route into the void.
            log!(
                "ioapic: id={} at {:#x} IGNORED — version register {:#010x} is not a redirection table",
                entry.id,
                entry.address,
                ver
            );
            continue;
        }
        let mut masked = 0;
        for n in 0..entries {
            pair.write(REG_REDTBL + 2 * n, RTE_MASKED);
            // Read back rather than trust the write: an unmasked entry is the hazard this loop exists to prevent.
            if pair.read(REG_REDTBL + 2 * n) & RTE_MASKED != 0 {
                masked += 1;
            }
        }
        drop(pair);
        drop(irq);
        unit.entries = entries;
        unit.lows = (0..entries).map(|_| AtomicU32::new(0)).collect();
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
        units.push(unit);
    }

    // One line for the whole table: the no-UART log tail holds a fixed number of rows.
    let mut table = String::new();
    for iso in &madt.source_overrides {
        let line = toyos_acpi::isa_line(iso.source_irq, &madt.source_overrides);
        let _ = write!(
            table,
            "{}{}:{}->{} {}",
            if table.is_empty() { "" } else { ", " },
            iso.bus,
            iso.source_irq,
            iso.gsi,
            describe(line.trigger, line.polarity)
        );
    }
    log!("ioapic: iso bus:irq->gsi [{}]", table);

    if units.is_empty() {
        log!("ioapic: none in MADT — no pin interrupts on this machine");
    }
    let topology = Box::leak(Box::new(Topology { units, overrides: madt.source_overrides.clone() }));
    let was = TOPOLOGY.swap(topology, Ordering::Release);
    assert!(was.is_null(), "ioapic: init ran twice");
}

pub fn describe(trigger: Trigger, polarity: Polarity) -> &'static str {
    match (trigger, polarity) {
        (Trigger::Edge, Polarity::High) => "edge/high",
        (Trigger::Edge, Polarity::Low) => "edge/low",
        (Trigger::Level, Polarity::High) => "level/high",
        (Trigger::Level, Polarity::Low) => "level/low",
    }
}

fn resolved(line: toyos_acpi::Line) -> Option<IsaLine> {
    if topology().units.is_empty() {
        return None;
    }
    Some(IsaLine { gsi: Gsi(line.gsi), trigger: line.trigger, polarity: line.polarity })
}

/// Where ISA `irq` lands and how it is driven, or `None` when no I/O APIC exists.
pub fn gsi_for_isa_irq(irq: u8) -> Option<IsaLine> {
    resolved(toyos_acpi::isa_line(irq, &topology().overrides))
}

/// Where the FADT's `SCI_INT` lands and how it is driven, or `None` when no
/// I/O APIC exists.
pub fn sci(sci_int: u16) -> Option<IsaLine> {
    resolved(toyos_acpi::sci_line(sci_int, &topology().overrides))
}

/// Every chip this machine routes pins through, by MADT id: the IOMMU needs the
/// whole set before it can decide anything, and decides once for the machine.
pub fn ids() -> Vec<u8> {
    topology().units.iter().map(|u| u.id).collect()
}

fn locate(gsi: Gsi) -> Result<(&'static Unit, u32), RouteError> {
    topology()
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
    let (unit, n) = locate(gsi)?;
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
            Delivery::Refused(why) => return Err(RouteError::NotRemappable(gsi, why)),
        };
    let low = vector as u32
        | index
        | if polarity == Polarity::Low { RTE_POLARITY_LOW } else { 0 }
        | if level { RTE_TRIGGER_LEVEL } else { 0 };
    let (read_low, read_high) = {
        let irq = IrqGuard::close();
        let pair = unit.registers.lock(&irq);
        // Destination first: writing the low word last means it is never briefly armed at the old destination.
        pair.write(REG_REDTBL + 2 * n + 1, high);
        pair.write(REG_REDTBL + 2 * n, low | RTE_MASKED);
        unit.lows[n as usize].store(low, Ordering::Relaxed);
        // Delivery status (12) and remote IRR (14) are the chip's, not ours.
        let read_low = pair.read(REG_REDTBL + 2 * n) & !(RTE_DELIVERY_STATUS | RTE_REMOTE_IRR);
        (read_low, pair.read(REG_REDTBL + 2 * n + 1))
    };
    let wrote_low = low | RTE_MASKED;
    if read_low != wrote_low || read_high != high {
        return Err(RouteError::Readback {
            wrote: u64::from(high) << 32 | u64::from(wrote_low),
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

/// Mask or unmask a pin [`route`] placed: one write of the low word it kept.
/// Callable from the pin's own interrupt handler.
pub fn set_masked(gsi: Gsi, masked: bool) -> Result<(), RouteError> {
    let (unit, n) = locate(gsi)?;
    let irq = IrqGuard::close();
    let pair = unit.registers.lock(&irq);
    // Under the pair, which `route` stored it under.
    let low = unit.lows[n as usize].load(Ordering::Relaxed);
    assert!(low != 0, "ioapic: gsi {} masked or unmasked before it was routed", gsi.0);
    pair.write(REG_REDTBL + 2 * n, if masked { low | RTE_MASKED } else { low });
    Ok(())
}
