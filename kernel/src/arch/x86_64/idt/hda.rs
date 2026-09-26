use super::device_irq::device_irq_entry;

// Lock-free, heap-free: may interrupt a CPU holding the controller lock (preemption disabled, not interrupts).
extern "sysv64" fn hda_handler() {
    crate::irq_census::irq_took!(Hda);
    crate::drivers::hda::isr_complete();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// HDA stream MSI entry (see `device_irq_entry` for the asm contract).
    pub(super) fn hda_entry => hda_handler
}
