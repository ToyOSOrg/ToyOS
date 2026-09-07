//! The vectors a PCI function a *process* drives delivers on.
//!
//! One per claim slot, because the slot is what the kernel needs to know: the
//! count goes to that slot's own record, and the wake it earns goes to that
//! slot's watchers and nobody else's. A single shared vector would
//! wake every user driver in the machine on any of their interrupts, which is
//! one process learning when another's device is busy.
//!
//! Lock-free and heap-free like every other device entry here: the record is
//! atomics and the wake happens on the next scheduler pass.

use super::device_irq::device_irq_entry;
use crate::irq_ring::IrqSource;

fn took(slot: usize) {
    crate::irq_census::irq_took!(UserDev);
    crate::pcidev::isr(slot);
    crate::irq_ring::isr_publish(IrqSource::UserDev, crate::clock::nanos_since_boot());
    // Force resched now, so `drain_irqs` turns the record into a wake before
    // the next quantum tick rather than after it.
    crate::preempt::set_need_resched();
    crate::arch::apic::eoi();
}

/// One handler and one entry per slot. A macro because the slot has to be an
/// immediate in the handler rather than a value read from somewhere: an ISR
/// that had to look up which device it was would be taking a lock.
macro_rules! user_dev_vectors {
    ($($handler:ident, $entry:ident => $slot:literal;)+) => {
        $(
            extern "sysv64" fn $handler() {
                took($slot);
            }

            device_irq_entry! {
                /// A claimed PCI function's MSI-X entry (see `device_irq_entry`
                /// for the asm contract).
                pub(super) fn $entry => $handler
            }
        )+

        /// As many entries as `pcidev` has slots; a mismatch would be a claim
        /// whose vector nothing answers.
        const _: () = assert!(
            [$($slot),+].len() == crate::pcidev::MAX_FUNCTIONS,
            "every claim slot needs a vector of its own",
        );
    };
}

user_dev_vectors! {
    user_dev0_handler, user_dev0_entry => 0;
    user_dev1_handler, user_dev1_entry => 1;
    user_dev2_handler, user_dev2_entry => 2;
    user_dev3_handler, user_dev3_entry => 3;
}
