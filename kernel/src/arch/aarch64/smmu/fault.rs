//! The event queue, drained from the unit's wired event interrupt and never
//! polled.
//!
//! The handler allocates nothing and takes no lock: what it reads is
//! published once, whole, before the interrupt is enabled, and what it writes
//! is atomics. For each record it first clears Bus Master Enable on the
//! function the record names — a function that cannot master the bus raises
//! no second event, which is the ceiling on a storm (PCI 3.0 §6.2.2, bit 2 of
//! `COMMAND`) — then counts it and hands it to whoever drives the function.
//!
//! **Who that is decides what the record ends.** A stream a process drives
//! has an owner: `pcidev` is told, that claim refuses every later call, and
//! the machine goes on. A stream no process drives has nobody to hand it to,
//! so after the line the machine halts, as x86-64's VT-d handler does.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use toyos_smmu::queue::{event, Events};
use toyos_smmu::unit as reg;

use super::Registers;
use crate::drivers::pci::{self, PciDevice};
use crate::iommu::StreamId;
use crate::log;
use crate::mm::{DirectMap, Mmio, PAGE_SIZE};

/// A `pcidev` slot number no claim has: this function is not driven by one.
const NO_SLOT: u32 = u32::MAX;

/// One function the unit translates for.
struct Function {
    function: StreamId,
    stream: u32,
    /// Physical base of its config window, through which an event clears `BME`.
    config: u64,
    domain: AtomicU32,
    faults: AtomicU32,
    /// The `pcidev` slot a process drives this function on, or [`NO_SLOT`].
    user_slot: AtomicU32,
}

/// What the handler reads, published once by [`arm`].
struct Armed {
    regs: Registers,
    queue: Mmio,
    events: Events,
    functions: Box<[Function]>,
    /// Events this unit has recorded for the life of the boot.
    count: AtomicU32,
    /// The StreamID of the first, or `u32::MAX`.
    first: AtomicU32,
}

static ARMED: AtomicPtr<Armed> = AtomicPtr::new(core::ptr::null_mut());

fn armed() -> Option<&'static Armed> {
    // SAFETY: non-null only as `arm` stored it: a leaked box, never freed,
    // published with Release once it was whole.
    unsafe { ARMED.load(Ordering::Acquire).as_ref() }
}

/// Everything the handler reads, before the unit's event interrupt is
/// enabled: the registers, the queue and its indexes, and each function
/// `routes` names.
pub(super) fn arm(regs: Registers, queue: Mmio, events: Events, devices: &[PciDevice], routes: &[(StreamId, u32)]) {
    let functions: Vec<Function> = routes
        .iter()
        .map(|&(function, stream)| {
            let device = devices
                .iter()
                .find(|d| StreamId::pci(d.bus, d.dev, d.func) == function)
                .expect("a route is an enumerated function's");
            Function {
                function,
                stream,
                config: DirectMap::phys_of(device.config_window().addr() as *const u8),
                domain: AtomicU32::new(0),
                faults: AtomicU32::new(0),
                user_slot: AtomicU32::new(NO_SLOT),
            }
        })
        .collect();
    let armed = Box::new(Armed {
        regs,
        queue,
        events,
        functions: functions.into_boxed_slice(),
        count: AtomicU32::new(0),
        first: AtomicU32::new(u32::MAX),
    });
    let first = ARMED.compare_exchange(core::ptr::null_mut(), Box::leak(armed), Ordering::Release, Ordering::Relaxed);
    assert!(first.is_ok(), "SMMU: the event handler was armed twice");
}

fn find(stream: u32) -> Option<&'static Function> {
    armed()?.functions.iter().find(|f| f.stream == stream)
}

fn by_function(function: StreamId) -> Option<&'static Function> {
    armed()?.functions.iter().find(|f| f.function == function)
}

/// Record which domain stream `stream` moved to, for the line.
pub(super) fn attached(stream: u32, domain: u16) {
    if let Some(function) = find(stream) {
        function.domain.store(u32::from(domain), Ordering::Relaxed);
    }
}

/// Record that a process drives `function` on `slot`, or no longer does.
pub fn user_owned(function: StreamId, slot: Option<usize>) {
    if let Some(function) = by_function(function) {
        function.user_slot.store(slot.map_or(NO_SLOT, |slot| slot as u32), Ordering::Release);
    }
}

/// The events the unit has recorded, as the handler has read them.
#[cfg(feature = "boot-actuators")]
pub(super) fn recorded() -> u32 {
    armed().map_or(0, |armed| armed.count.load(Ordering::Relaxed))
}

/// The unit's event interrupt.
pub fn service() {
    let armed = armed().expect("SMMU: the event interrupt is enabled only once the handler is armed");
    let (regs, events) = (armed.regs, armed.events);
    let mut kernel_owned = 0usize;
    let mut prod = regs.read(reg::EVENTQ_PROD);
    let mut cons = regs.read(reg::EVENTQ_CONS);
    if events.overflowed(prod, cons) {
        log!("iommu: the SMMUv3's event queue overflowed: earlier events are lost");
    }
    // Every record the unit wrote before the last read of `PROD`, one that
    // lands after it raising the interrupt again; at most twice a queue's
    // worth, so a stream no `BME` stops cannot hold this CPU here.
    let mut budget = 2u32 << super::EVENTS_LOG2;
    while !events.is_empty(prod, cons) && budget > 0 {
        budget -= 1;
        let at = events.slot(cons) as u64 * 32;
        let record = [0, 8, 16, 24].map(|offset| armed.queue.read_u64(at + offset));
        if record_one(armed, event(record)) {
            kernel_owned += 1;
        }
        cons = events.after(cons, prod);
        regs.write(reg::EVENTQ_CONS, cons);
        prod = regs.read(reg::EVENTQ_PROD);
    }
    let errors = reg::active_errors(regs.read(reg::GERROR), regs.read(reg::GERRORN));
    if errors & reg::GERROR_EVENTQ_ABORT != 0 {
        log!("iommu: the SMMUv3 could not write its event queue: events are lost (GERROR {errors:#x})");
    }
    if kernel_owned > 0 {
        // A function reached an address this kernel never gave it, and
        // nothing here can know what else it did: the line is the report,
        // and the machine stops.
        crate::drivers::panic_console::capture();
        crate::panic::halt_all_cpus();
    }
}

/// One record, handed to whoever drives the function it names; `true` where
/// that is nobody.
fn record_one(armed: &Armed, event: toyos_smmu::queue::Event) -> bool {
    let function = find(event.stream);
    // First, before anything that can be slow: the ceiling on a storm.
    if let Some(function) = function {
        pci::stop_bus_mastering(config_window(function.config));
    }
    let faults = function.map_or(0, |f| f.faults.fetch_add(1, Ordering::Relaxed) + 1);
    let count = armed.count.fetch_add(1, Ordering::Relaxed) + 1;
    let first = armed.first.compare_exchange(u32::MAX, event.stream, Ordering::AcqRel, Ordering::Relaxed).is_ok();
    let owner = function.map(|f| f.user_slot.load(Ordering::Acquire)).filter(|slot| *slot != NO_SLOT);
    if let Some(slot) = owner {
        crate::pcidev::note_fault(slot as usize);
    }
    let (address, access) = match event.attempt {
        Some(attempt) => (attempt.address, if attempt.write { "write" } else { "read" }),
        None => (0, "none"),
    };
    log!(
        // `owner=` first: `tests/common/serial.rs` reads `owner=kernel` as a
        // death and the other form as a record. The event's name is the last
        // word.
        "iommu: DMA FAULT owner={} unit0 stream={} addr={address:#018x} access={access} domain={} bme={} \
         unitfaults={count} streamfaults={faults} first={} reason={:?} {}",
        Owner(owner),
        Who(function, event.stream),
        function.map_or(0, |f| f.domain.load(Ordering::Relaxed)),
        if function.is_some() { "cleared" } else { "unknown-function" },
        if first { 'y' } else { 'n' },
        event.code,
        event.code.name(),
    );
    owner.is_none()
}

/// The function's config window, from the address [`arm`] published.
fn config_window(phys: u64) -> Mmio {
    // SAFETY: `phys` is `DirectMap::phys_of` a config window the ECAM mapping
    // holds for the machine's life, one page wide.
    unsafe { Mmio::over_phys(DirectMap::from_phys(phys), PAGE_SIZE) }
}

/// Who an event was handed to.
struct Owner(Option<u32>);

impl core::fmt::Display for Owner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            None => write!(f, "kernel"),
            Some(slot) => write!(f, "slot{slot}"),
        }
    }
}

/// The function an event names, as `pci::enumerate` prints it, or the raw
/// StreamID where it is none this unit translates for.
struct Who(Option<&'static Function>, u32);

impl core::fmt::Display for Who {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(function) => write!(f, "{}", function.function),
            None => write!(f, "{:#x}", self.1),
        }
    }
}
