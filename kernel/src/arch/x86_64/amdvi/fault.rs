//! The event log, drained on the unit's own interrupt: bounded by the log's
//! 256 records, allocating nothing and taking no lock; unit state lives in a
//! fixed array of atomics, published before the unit is switched on. What a
//! record ends is `crate::iommu::fault`'s.
//!
//! A record no requester caused — an entry or command of this driver's the
//! unit refused, or the unit's own hardware failing — is a panic: past it
//! nothing can state what the unit lets through.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::iommu::fault::{self as policy, Access, Fault, Who};
use crate::log;
use crate::mm::{DirectMap, Mmio, PAGE_SIZE};
use toyos_amdvi::queue::{event, name, Event, ENTRIES};
use toyos_amdvi::reg::{self, control, status};

use super::{requester, spin_until, REGISTER_WINDOW};
use crate::arch::x86_64::iommu_unit::MAX_UNITS;

struct FaultUnit {
    /// Physical base of the register window; 0 means this slot is unused.
    regs: AtomicU64,
    /// Physical base of the event log.
    log: AtomicU64,
    /// Faults this unit has reported for the life of the boot.
    faults: AtomicU32,
}

static UNITS: [FaultUnit; MAX_UNITS] =
    [const { FaultUnit { regs: AtomicU64::new(0), log: AtomicU64::new(0), faults: AtomicU32::new(0) } }; MAX_UNITS];

/// Publish unit `index`'s registers and event log to the handler, before its
/// interrupt is armed.
pub(super) fn arm(index: usize, regs: Mmio, log: u64) {
    UNITS[index].log.store(log, Ordering::Relaxed);
    // Last, with Release: the handler tests this field first.
    UNITS[index].regs.store(DirectMap::phys_of(regs.addr() as *const u8), Ordering::Release);
}

/// `bytes` at `phys`, through the direct map.
fn window(phys: u64, bytes: u64) -> Mmio {
    // SAFETY: `phys` is a register window `paging::map_mmio` mapped at boot
    // and never unmapped, or an event log `pmm` handed out and the unit
    // writes for the machine's life; the direct map covers both.
    unsafe { Mmio::over_phys(DirectMap::from_phys(phys), bytes) }
}

/// A unit raised its event interrupt.
pub fn service() {
    let mut kernel_owned = 0usize;
    for (index, unit) in UNITS.iter().enumerate() {
        let regs = unit.regs.load(Ordering::Acquire);
        if regs != 0 {
            kernel_owned += drain(index, window(regs, REGISTER_WINDOW), window(unit.log.load(Ordering::Relaxed), PAGE_SIZE));
        }
    }
    policy::conclude(kernel_owned);
    crate::arch::apic::eoi();
}

/// Every record from head to tail, of which the answer counts only the ones
/// on an enumerated function this kernel drives.
fn drain(index: usize, regs: Mmio, log: Mmio) -> usize {
    // Cleared first, so a record written after the tail is read raises the
    // interrupt again (MMIO 2020h `EventLogInt`).
    let seen = regs.read_u32(reg::STATUS);
    regs.write_u32(reg::STATUS, seen & (status::EVENT_LOG_INT | status::EVENT_OVERFLOW));
    let tail = reg::slot(regs.read_u64(reg::EVENT_TAIL));
    let mut head = reg::slot(regs.read_u64(reg::EVENT_HEAD));
    let mut halts = 0;
    while head != tail {
        let at = reg::offset(head);
        let words = [log.read_u64(at), log.read_u64(at + 8)];
        let fault = match event(words) {
            Event::PageFault { device, address, write, .. } => Fault {
                who: Who::Function(requester(device)),
                address,
                access: if write { Access::Write } else { Access::Read },
                reason: 0b0010,
                name: name(0b0010),
            },
            Event::DeviceRequest { device, address } => Fault {
                who: Who::Function(requester(device)),
                address,
                access: Access::Unrecorded,
                reason: 0b1000,
                name: name(0b1000),
            },
            Event::Other { code } => panic!(
                "iommu: amdvi unit{index} recorded {} ({code:#x}): {:#018x}:{:#018x}",
                name(code),
                words[1],
                words[0]
            ),
        };
        halts += usize::from(policy::report(index, &UNITS[index].faults, fault));
        head = (head + 1) % ENTRIES;
    }
    regs.write_u64(reg::EVENT_HEAD, reg::offset(head));
    if seen & status::EVENT_OVERFLOW != 0 {
        restart(index, regs);
    }
    halts
}

/// The event log, logging again after it overflowed (§2.5.1): what it
/// dropped is said, never read.
fn restart(index: usize, regs: Mmio) {
    log!("iommu: amdvi unit{index} event log overflowed — earlier faults are lost");
    regs.write_u64(reg::CONTROL, regs.read_u64(reg::CONTROL) & !control::EVENT_LOG_EN);
    spin_until(
        || regs.read_u32(reg::STATUS) & status::EVENT_LOG_RUN == 0,
        || alloc::format!("iommu: amdvi unit{index} never stopped its event log: STATUS={:#010x}", regs.read_u32(reg::STATUS)),
    );
    regs.write_u32(reg::STATUS, status::EVENT_OVERFLOW);
    regs.write_u64(reg::CONTROL, regs.read_u64(reg::CONTROL) | control::EVENT_LOG_EN);
}

/// Records every unit has reported, for the selftest to wait on.
#[cfg(feature = "boot-actuators")]
pub(super) fn recorded() -> u32 {
    UNITS.iter().map(|unit| unit.faults.load(Ordering::Relaxed)).sum()
}
