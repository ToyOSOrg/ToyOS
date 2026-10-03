//! The vector a claimed ISA function's lines deliver on: a count into the
//! row's record and a post of its claim's watch, as a claimed PCI function's
//! message is.

use super::device_irq::device_irq_entry;

extern "sysv64" fn isa0_handler() {
    crate::arch::percpu::irq_took!(UserDev);
    crate::isa::isr(0);
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::isa_claim_straddles_quarantine() {
        crate::arch::i8042::straddle::stage_late_edge();
    }
    crate::preempt::set_need_resched();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// `pio::GRANTABLE`'s row 0.
    pub(super) fn isa0_entry => isa0_handler
}
