use super::device_irq::device_irq_entry;

// Lock-free and heap-free: may interrupt a CPU holding the controller lock, which disables preemption but not interrupts.
extern "sysv64" fn virtio_sound_handler() {
    crate::irq_census::irq_took!(Sound);
    crate::drivers::virtio_sound::isr_complete();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// Virtio-sound MSI-X entry (see `device_irq_entry` for the asm contract).
    pub(super) fn virtio_sound_entry => virtio_sound_handler
}
