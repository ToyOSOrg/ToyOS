//! Interrupt remapping (AMD 48882 rev 3.11 §2.2.5): a table of 256 entries
//! per requester, indexed by vector, which the unit picks by that requester's
//! own device table entry.
//!
//! A fixed message's data bits 10:0 index its requester's table (Figure 14),
//! so a function is armed with the message it would carry with no unit at all,
//! and the entry at its vector says where that vector goes. Source
//! verification is the table's own: a requester reaches only the vectors its
//! table holds present, and a message on any other is target-aborted and
//! recorded (`RemapEn` 0, §2.2.5.2 step 7).
//!
//! Every entry is the 128-bit format, `CONTROL.GAEn` with `GAMEn` 000b and
//! `GuestMode` 0 (Table 21, Figure 17); its destination is 32 bits wide under
//! `XTEn` (§2.2.5.3, Figure 19), which an x2APIC machine needs.
//!
//! This module's lock is taken before `DOMAINS`, `UNITS` and `TABLES`, never
//! after any of them.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use toyos_amdvi::dte::{interrupts_remapped, ENTRY_BYTES};
use toyos_amdvi::irte::{self, Destination};
use toyos_amdvi::queue::Command;
use toyos_phys::Phys as Address;

use super::super::apic::{msi_message, narrow_destination, MSG_DEST, MSG_DEST_NARROW};
use super::super::iommu_unit::interrupt::{Msi, Pin, Refused};
use super::{domain, memory, submit, DEVICE_TABLE, TABLES};
use crate::iommu::StreamId;
use crate::log;
use crate::sync::Lock;
use core::sync::atomic::{AtomicBool, Ordering};

struct Remap {
    /// Whether every unit remaps, decided once at boot: `None` leaves every
    /// entry's interrupt half invalid and every message as written.
    extended: Option<bool>,
    /// Each requester's table, made on its first entry and kept.
    tables: BTreeMap<u16, Address<12>>,
    /// The requester id firmware's IVRS gives each I/O APIC (§5.2.2, Table 109).
    apics: Vec<(u8, u16)>,
}

static REMAP: Lock<Remap> = Lock::new(Remap { extended: None, tables: BTreeMap::new(), apics: Vec::new() });

/// Every programmed unit filters SMIs, from [`super::settle`] on.
static SMI_FILTERED: AtomicBool = AtomicBool::new(false);

pub(super) fn smi_filtered(everywhere: bool) {
    SMI_FILTERED.store(everywhere, Ordering::Release);
}

pub fn smi_filtered_everywhere() -> bool {
    SMI_FILTERED.load(Ordering::Acquire)
}

/// Every unit remaps, with an entry's destination 32 bits wide where
/// `extended`; the I/O APICs speak as `apics`.
pub(super) fn start(extended: bool, apics: Vec<(u8, u16)>) {
    let mut remap = REMAP.lock();
    remap.extended = Some(extended);
    remap.apics = apics;
}

pub fn is_armed() -> bool {
    REMAP.lock().extended.is_some()
}

/// The interrupt half of requester `id`'s entry, bits 191:128.
pub(super) fn interrupt_half(id: u16) -> crate::mm::Mmio {
    let table = DEVICE_TABLE.load(Ordering::Acquire);
    memory(table + u64::from(id) * ENTRY_BYTES as u64 + 16, 8)
}

impl Remap {
    /// Entry `vector` of `id`'s table delivering it to `destination`, or
    /// absent; the table made and named by `id`'s entry first where it has
    /// none. Published to every unit before this returns.
    fn write(&mut self, id: u16, vector: u8, entry: [u64; 2]) -> [u64; 2] {
        let table = match self.tables.get(&id) {
            Some(table) => *table,
            None => {
                let table = TABLES.lock().alloc();
                self.tables.insert(id, table);
                // One 64-bit write of a half whose `IV` is already set, then
                // the entry invalidated (§2.2.2.2).
                interrupt_half(id).write_u64(0, interrupts_remapped(table, domain::setting(id)));
                submit(&[Command::InvalidateDevice(id)]);
                table
            }
        };
        let slot = memory(table.get() + u64::from(vector) * 16, 16);
        // `RemapEn` is in the low doubleword: it goes last when an entry
        // becomes present, and first when it stops being.
        if irte::present(entry) {
            slot.write_u64(8, entry[1]);
            slot.write_u64(0, entry[0]);
        } else {
            slot.write_u64(0, entry[0]);
            slot.write_u64(8, entry[1]);
        }
        submit(&[Command::InvalidateInterrupts(id)]);
        [slot.read_u64(0), slot.read_u64(8)]
    }

    fn destination(&self, apic: u32) -> Result<Destination, Refused> {
        match self.extended {
            Some(true) => Ok(Destination::Extended(apic)),
            Some(false) => narrow_destination(apic).map(Destination::Narrow).map_err(|_| Refused::DestinationTooWide(apic)),
            None => unreachable!("amdvi: an entry written on a machine that does not remap"),
        }
    }
}

/// What an entry holds, read back out of the table.
fn report(source: StreamId, vector: u8, read: [u64; 2]) {
    log!(
        "iommu: amdvi irte source={source} vector={vector:#04x} p={} dst={:#x} delivers={:#04x}",
        u8::from(irte::present(read)),
        irte::destination(read),
        irte::vector(read),
    );
}

/// `source`'s entry at `vector`, delivering it to this kernel's CPU.
fn arm(source: StreamId, vector: u8) {
    let mut remap = REMAP.lock();
    let destination = match remap.extended {
        Some(true) => Destination::Extended(MSG_DEST),
        _ => Destination::Narrow(MSG_DEST_NARROW),
    };
    let read = remap.write(source.requester(), vector, irte::entry(vector, destination));
    drop(remap);
    report(source, vector, read);
}

/// A claim's entry at `vector` for `source`, whose entry from now on passes
/// nothing firmware declared for it and gives it no `SysMgt`.
pub(in crate::arch::x86_64) fn claim(vector: u8, source: StreamId) -> Result<Msi, Refused> {
    // A requester the platform speaks as is refused before here
    // (`iommu::isolates`), and one a kernel driver holds by `pcidev::claim`.
    // A table already written is a re-claim's, whose setting is `CLAIMED`;
    // any other is one of those, whose entries keep firmware's pass bits.
    assert!(
        !REMAP.lock().tables.contains_key(&source.requester()) || domain::setting(source.requester()) == toyos_amdvi::dte::Setting::CLAIMED,
        "amdvi: a claim on {source}, whose remapping table another source already wrote"
    );
    domain::claimed(source.requester());
    if is_armed() {
        arm(source, vector);
    }
    let (address, data) = msi_message(vector);
    Ok(Msi { address, data })
}

/// A claim's entry absent again and gone from every unit's cache.
pub(in crate::arch::x86_64) fn release(vector: u8, source: StreamId) {
    let read = REMAP.lock().write(source.requester(), vector, irte::ABSENT);
    log!("iommu: amdvi irte source={source} vector={vector:#04x} p={} released", u8::from(irte::present(read)));
}

/// An I/O APIC pin's entry at its own vector, so its redirection entry stays
/// in the format it has with no unit: a level-triggered pin's EOI then names
/// the vector the CPU took.
pub(in crate::arch::x86_64) fn pin(apic_id: u8, vector: u8, dest: u32) -> Result<Pin, Refused> {
    let mut remap = REMAP.lock();
    let id = remap.apics.iter().find(|(apic, _)| *apic == apic_id).map(|(_, id)| *id);
    let id = id.ok_or(Refused::ControllerUnnamed(apic_id))?;
    let entry = irte::entry(vector, remap.destination(dest)?);
    let read = remap.write(id, vector, entry);
    drop(remap);
    let [devfn, bus] = id.to_le_bytes();
    report(StreamId::pci(bus, devfn >> 3, devfn & 7), vector, read);
    // The entry's destination is what the unit delivers to; the redirection
    // entry's is never read.
    Ok(Pin { low: 0, high: 0 })
}

/// The rule [`super::init`] decides remapping by, over what the units and
/// firmware said: `None`, said, where any requirement is missing.
pub(super) fn remappable(guest_apic: bool, x2apic_units: bool, x2apic_cpus: bool, named: &[(u8, u16)], apics: &[u8]) -> Option<bool> {
    if !guest_apic {
        log!("iommu: an AMD-Vi unit has GASup clear, so no message is remapped on this machine");
        return None;
    }
    if x2apic_cpus && !x2apic_units {
        log!("iommu: the CPUs are in x2APIC and an AMD-Vi unit has XTSup clear, so no message is remapped");
        return None;
    }
    if let Some(apic) = apics.iter().find(|apic| !named.iter().any(|(named, _)| named == *apic)) {
        log!(
            "iommu: no programmed AMD-Vi unit names I/O APIC {apic} in the IVRS, so its pins could not be remapped \
             and no message is"
        );
        return None;
    }
    Some(x2apic_cpus)
}
