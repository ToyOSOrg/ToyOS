//! The vector a claimed ISA function's lines deliver on: a count into the
//! row's record and a pass owed, as a claimed PCI function's message is.

use super::device_irq::device_irq_entry;
use crate::irq_ring::IrqSource;

extern "sysv64" fn isa0_handler() {
    crate::arch::percpu::irq_took!(UserDev);
    crate::isa::isr(0);
    crate::irq_ring::isr_publish(IrqSource::UserDev, crate::clock::nanos_since_boot());
    crate::preempt::set_need_resched();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// `pio::GRANTABLE`'s row 0.
    pub(super) fn isa0_entry => isa0_handler
}
