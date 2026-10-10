//! What a unit's fault handler does with each record it reads, whatever the
//! unit: bounded, allocating nothing and taking no lock. Function state lives
//! in a slice of exactly the enumerated functions, published once before any
//! unit is armed.
//!
//! Whatever the stream, the same things happen first: Bus Master Enable
//! cleared on the function that faulted, the first faulting function latched,
//! and a count kept per unit and per function. Clearing `BME` is also the
//! ceiling on a storm, since a function that cannot master the bus cannot
//! raise a second fault (PCI 3.0 §6.2.2, bit 2 of `COMMAND`).
//!
//! **What differs is who the fault is handed to.** An enumerated function
//! every driver of which is in this kernel has nobody, and its fault is a
//! defect of this kernel, so the terminal action is a halt — the last thing
//! that happens rather than the first. A function a process drives has an
//! owner: `pcidev` is told, that claim refuses every later call, and the
//! machine goes on, because one process's bug taking the machine down is the
//! thing moving a driver out of the kernel was for. A record that names no
//! enumerated function is a device's input this kernel never took on: the
//! unit has already refused it, so it is counted and logged and the machine
//! goes on. Nothing here can clear such a requester's `BME`, so a backend
//! bounds what it reads per interrupt by itself, never by the device.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use crate::drivers::pci::{self, PciDevice};
use crate::iommu::StreamId;
use crate::mm::{DirectMap, Mmio, PAGE_SIZE};

/// A `pcidev` slot number no claim has: this function is not driven by one.
const NO_SLOT: u32 = u32::MAX;

/// One enumerated function, published before any unit is armed.
struct Function {
    who: StreamId,
    /// Physical base of its config window, through which a fault clears `BME`.
    config: u64,
    domain: AtomicU32,
    // Faults a unit has reported against it; non-zero is the per-domain flag.
    faults: AtomicU32,
    /// The `pcidev` slot a process drives this function on, or [`NO_SLOT`].
    /// What decides whether a fault on it is terminal for the machine.
    user_slot: AtomicU32,
}

/// Exactly the functions this machine enumerated, leaked once by [`describe`]:
/// null until then, and never written again.
static FUNCTIONS: AtomicPtr<&'static [Function]> = AtomicPtr::new(core::ptr::null_mut());

fn functions() -> &'static [Function] {
    let published = FUNCTIONS.load(Ordering::Acquire);
    if published.is_null() {
        return &[];
    }
    // SAFETY: non-null only as `describe` stored it: a leaked box, never freed
    // or written again, published with Release once it was whole.
    unsafe { *published }
}

/// [`Who::key`] of the first fault this machine took: what a later one says
/// is decided by what the first one already broke.
static FIRST: AtomicU64 = AtomicU64::new(u64::MAX);

/// Every function this machine enumerated, before any unit is armed: the
/// handler reaches a faulting function's config space through this, with no lock.
pub fn describe(devices: &[PciDevice]) {
    let functions: Vec<Function> = devices
        .iter()
        .map(|device| Function {
            who: StreamId::pci(device.bus, device.dev, device.func),
            config: DirectMap::phys_of(device.config_window().addr() as *const u8),
            domain: AtomicU32::new(0),
            faults: AtomicU32::new(0),
            user_slot: AtomicU32::new(NO_SLOT),
        })
        .collect();
    let functions: &'static [Function] = Box::leak(functions.into_boxed_slice());
    let first = FUNCTIONS.compare_exchange(
        core::ptr::null_mut(),
        Box::leak(Box::new(functions)),
        Ordering::Release,
        Ordering::Relaxed,
    );
    assert!(first.is_ok(), "iommu: the fault handler's functions were described twice");
}

/// Record which domain a function moved to, for the flag the handler sets.
pub fn attached(stream: StreamId, domain: u16) {
    if let Some(function) = find(Who::Function(stream)) {
        function.domain.store(u32::from(domain), Ordering::Relaxed);
    }
}

/// Record that a process drives this function on `slot`, or no longer does.
///
/// The handler reads it to decide what a fault on this stream is *terminal
/// for*: a kernel-driven function has nothing to hand a fault to and the
/// response is the halt in [`conclude`]; one a process drives has an owner, so
/// the record goes to that owner's claim and the machine stays up.
pub fn user_owned(stream: StreamId, slot: Option<usize>) {
    if let Some(function) = find(Who::Function(stream)) {
        function.user_slot.store(slot.map_or(NO_SLOT, |slot| slot as u32), Ordering::Release);
    }
}

fn find(who: Who) -> Option<&'static Function> {
    match who {
        Who::Function(stream) => functions().iter().find(|f| f.who == stream),
        Who::Stream(_) => None,
    }
}

/// Whom a record names.
#[derive(Clone, Copy)]
pub enum Who {
    /// A requester id: VT-d's source-id, or the function the IORT routes an
    /// SMMU StreamID from.
    Function(StreamId),
    /// An SMMU StreamID no enumerated function is routed from.
    // The SMMUv3 is AArch64's alone, so x86-64 builds a variant it never makes.
    #[allow(dead_code)]
    Stream(u32),
}

impl Who {
    /// One number per `Who`, none of them `u64::MAX`.
    const fn key(self) -> u64 {
        match self {
            Self::Function(stream) => stream.requester() as u64,
            Self::Stream(stream) => 1 << 32 | stream as u64,
        }
    }
}

impl core::fmt::Display for Who {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Function(stream) => write!(f, "{stream}"),
            Self::Stream(stream) => write!(f, "{stream:#x}"),
        }
    }
}

/// What the faulting transaction was, where the record says.
#[derive(Clone, Copy)]
pub enum Access {
    Read,
    Write,
    /// An SMMU event that carries no transaction.
    // The SMMUv3 is AArch64's alone, so x86-64 builds a variant it never makes.
    #[allow(dead_code)]
    Unrecorded,
}

impl core::fmt::Display for Access {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Unrecorded => "none",
        })
    }
}

/// One record a unit reported, decoded by its backend.
pub struct Fault {
    pub who: Who,
    pub address: u64,
    pub access: Access,
    /// The unit's own number for why, and its name.
    pub reason: u8,
    pub name: &'static str,
}

/// `fault`, read off unit `unit`, whose count of reported faults is `count`:
/// its function stopped, it is handed to whoever drives that function, and
/// its line written. `true` only for an enumerated function this kernel
/// drives, which the drain ends on with [`conclude`].
pub fn report(unit: usize, count: &AtomicU32, fault: Fault) -> bool {
    let function = find(fault.who);
    // First, before anything that can be slow or say no: a function that
    // cannot master the bus raises no second fault, which is the whole of
    // the ceiling on a storm.
    if let Some(function) = function {
        pci::stop_bus_mastering(config_window(function.config));
    }
    let seen_here = function.map_or(0, |f| f.faults.fetch_add(1, Ordering::Relaxed) + 1);
    let key = fault.who.key();
    let _ = FIRST.compare_exchange(u64::MAX, key, Ordering::AcqRel, Ordering::Relaxed);
    // Before the line, so `owner=` in it is what was actually told.
    let owner = match function.map(|f| f.user_slot.load(Ordering::Acquire)) {
        None => Owner::Nobody,
        Some(NO_SLOT) => Owner::Kernel,
        Some(slot) => Owner::Slot(slot as usize),
    };
    if let Owner::Slot(slot) = owner {
        crate::pcidev::note_fault(slot);
    }
    let count = count.fetch_add(1, Ordering::Relaxed) + 1;
    log!(
        // `owner=` first, because it is the only field that decides whether
        // this machine is still running: `tests/common/serial.rs` reads
        // `iommu: DMA FAULT owner=kernel` as a death and the other forms as
        // records. The reason's name is the last word: every gate takes it
        // from there.
        "iommu: DMA FAULT owner={} unit{unit} stream={} addr={:#018x} access={} reason={:#04x} \
         domain={} bme={} unitfaults={count} streamfaults={seen_here} first={} {}",
        owner,
        fault.who,
        fault.address,
        fault.access,
        fault.reason,
        Blamed(function),
        if function.is_some() { "cleared" } else { "unknown-function" },
        if FIRST.load(Ordering::Relaxed) == key { 'y' } else { 'n' },
        fault.name,
    );
    matches!(owner, Owner::Kernel)
}

/// The end of a drain that read `kernel_owned` faults on functions this kernel
/// drives: any at all halts the machine.
///
/// The halt is the whole response and there is no recovery missing from it: a
/// faulting device reached an address this kernel never gave it, and nothing
/// here can know what else it already did. **So this halts and never panics**
/// — a report of one carries the fault record and `panic_reboot`'s arm line,
/// and no `panicked at` line; `capture` puts the fault on the panel first.
///
/// **Only for a function this kernel drives.** A function a process drives has
/// an owner to refuse: its bus mastering is already gone by the time this is
/// reached, its claim answers every later call `Io`, and the machine — whose
/// other drivers are untouched — goes on. A requester nobody enumerated has
/// no driver here to be wrong, and the unit already refused what it sent.
pub fn conclude(kernel_owned: usize) {
    if kernel_owned > 0 {
        crate::drivers::panic_console::capture();
        crate::panic::halt_all_cpus();
    }
}

/// The config window of the function at `phys`, from the address [`describe`]
/// published.
fn config_window(phys: u64) -> Mmio {
    // SAFETY: `phys` is `DirectMap::phys_of` a config window the ECAM mapping
    // holds for the machine's life, one page wide.
    unsafe { Mmio::over_phys(DirectMap::from_phys(phys), PAGE_SIZE) }
}

/// Who a fault was handed to, in the line. A word rather than a number, because
/// which of them it is decides whether this machine is still running.
#[derive(Clone, Copy)]
enum Owner {
    /// An enumerated function only this kernel drives: the halt.
    Kernel,
    /// An enumerated function the process on this `pcidev` slot drives.
    Slot(usize),
    /// No enumerated function: a requester this kernel never took on.
    Nobody,
}

impl core::fmt::Display for Owner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel => write!(f, "kernel"),
            Self::Slot(slot) => write!(f, "slot{slot}"),
            Self::Nobody => write!(f, "none"),
        }
    }
}

/// What the handler can say about the faulting function's address space. The
/// middle answer is deliberately weak: a function the kernel never attached
/// carries domain id 0 whether it sits on the unit's default or on a domain an
/// actuator bound by hand, so the label claims only that nothing recorded one.
struct Blamed(Option<&'static Function>);

impl core::fmt::Display for Blamed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0.map(|function| function.domain.load(Ordering::Relaxed)) {
            None => write!(f, "unknown"),
            Some(0) => write!(f, "unrecorded"),
            Some(id) => write!(f, "{id}"),
        }
    }
}
