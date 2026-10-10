//! The event queue, drained from the unit's wired event interrupt and never
//! polled.
//!
//! The handler allocates nothing and takes no lock: what it reads is
//! published once, whole, before the interrupt is enabled, and what it writes
//! is atomics. What a record ends is `crate::iommu::fault`'s, the policy every
//! backend applies; this reads the queue into it.
//!
//! One interrupt reads at most a queue's worth of records, so a device that
//! never stops writing, which nothing here can stop when no enumerated
//! function is behind its stream, cannot hold the CPU. The unit pulses its
//! interrupt only as the queue goes from empty to non-empty (IHI 0070 H.a
//! §3.18.2), so a record left behind raises nothing: a drain that stops short
//! of empty pends the interrupt again itself.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use toyos_smmu::queue::{event, Events};
use toyos_smmu::unit as reg;

use super::Registers;
use crate::iommu::fault::{self as policy, Access, Fault, Who};
use crate::iommu::StreamId;
use crate::log;
use crate::mm::Mmio;

/// What the handler reads, published once by [`arm`].
struct Armed {
    regs: Registers,
    queue: Mmio,
    events: Events,
    /// Each enumerated function the IORT routes through the unit, and its
    /// StreamID.
    routes: Box<[(StreamId, u32)]>,
    /// Events this unit has recorded for the life of the boot.
    faults: AtomicU32,
}

static ARMED: AtomicPtr<Armed> = AtomicPtr::new(core::ptr::null_mut());

fn armed() -> Option<&'static Armed> {
    // SAFETY: non-null only as `arm` stored it: a leaked box, never freed,
    // published with Release once it was whole.
    unsafe { ARMED.load(Ordering::Acquire).as_ref() }
}

/// Everything the handler reads, before the unit's event interrupt is
/// enabled: the registers, the queue and its indexes, and the routes.
pub(super) fn arm(regs: Registers, queue: Mmio, events: Events, routes: &[(StreamId, u32)]) {
    let armed =
        Box::new(Armed { regs, queue, events, routes: routes.into(), faults: AtomicU32::new(0) });
    let first = ARMED.compare_exchange(core::ptr::null_mut(), Box::leak(armed), Ordering::Release, Ordering::Relaxed);
    assert!(first.is_ok(), "SMMU: the event handler was armed twice");
}

/// The events the unit has recorded, as the handler has read them.
#[cfg(feature = "boot-actuators")]
pub(super) fn recorded() -> u32 {
    armed().map_or(0, |armed| armed.faults.load(Ordering::Relaxed))
}

/// The unit's event interrupt.
pub fn service() {
    let armed = armed().expect("SMMU: the event interrupt is enabled only once the handler is armed");
    let (regs, events) = (armed.regs, armed.events);
    let mut kernel_owned = 0usize;
    let mut cons = regs.read(reg::EVENTQ_CONS);
    let mut prod = regs.read(reg::EVENTQ_PROD);
    for _ in 0..events.entries() {
        if events.is_empty(prod, cons) {
            break;
        }
        if events.overflowed(prod, cons) {
            log!("iommu: the SMMUv3's event queue overflowed: earlier events are lost");
        }
        let at = events.slot(cons) as u64 * 32;
        let record = event([0, 8, 16, 24].map(|offset| armed.queue.read_u64(at + offset)));
        let who = match armed.routes.iter().find(|(_, stream)| *stream == record.stream) {
            Some(&(function, _)) => Who::Function(function),
            None => Who::Stream(record.stream),
        };
        let (address, access) = match record.attempt {
            Some(attempt) => (attempt.address, if attempt.write { Access::Write } else { Access::Read }),
            None => (0, Access::Unrecorded),
        };
        let fault = Fault { who, address, access, reason: record.number, name: record.code.name() };
        if policy::report(0, &armed.faults, fault) {
            kernel_owned += 1;
        }
        cons = events.after(cons, prod);
        regs.write(reg::EVENTQ_CONS, cons);
        prod = regs.read(reg::EVENTQ_PROD);
    }
    if !events.is_empty(prod, cons) {
        super::super::irqchip::pend_iommu_events();
    }
    let errors = reg::active_errors(regs.read(reg::GERROR), regs.read(reg::GERRORN));
    if errors & reg::GERROR_EVENTQ_ABORT != 0 {
        log!("iommu: the SMMUv3 could not write its event queue: events are lost (GERROR {errors:#x})");
    }
    policy::conclude(kernel_owned);
}
