use super::device_irq::device_irq_entry;
use crate::irq_ring::IrqSource;

// Lock-free, heap-free: RX draining happens in `sched::driver::drain_irqs`, never here.
extern "sysv64" fn virtio_net_handler() {
    crate::irq_census::irq_took!(Net);
    let timestamp = crate::clock::nanos_since_boot();
    crate::irq_ring::isr_publish(IrqSource::Net, timestamp);
    // Force resched now so drain_irqs runs before the next quantum tick.
    crate::preempt::set_need_resched();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// Virtio-net MSI-X entry (see `device_irq_entry` for the asm contract).
    pub(super) fn virtio_net_entry => virtio_net_handler
}
