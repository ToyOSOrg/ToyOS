//! The vectors a claimed `isa` row's lines deliver on, one per row: a count
//! into the row's record and a post of its claim's watch, as a claimed PCI
//! function's message is.

use super::device_irq::device_irq_entry;

macro_rules! isa_row {
    ($handler:ident, $entry:ident, $row:literal) => {
        extern "sysv64" fn $handler() {
            crate::arch::percpu::irq_took!(UserDev);
            crate::isa::isr($row);
            crate::preempt::set_need_resched();
            crate::arch::apic::eoi();
        }

        device_irq_entry! {
            pub(super) fn $entry => $handler
        }
    };
}

isa_row!(isa0_handler, isa0_entry, 0);
isa_row!(isa1_handler, isa1_entry, 1);
