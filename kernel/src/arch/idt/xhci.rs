use super::device_irq::device_irq_entry;
use crate::irq_ring::IrqSource;

// Lock-free and heap-free — the event ring is polled by drivers::xhci::poll_if_pending, never here.
extern "sysv64" fn xhci_handler() {
    crate::irq_census::irq_took!(Xhci);
    let timestamp = crate::clock::nanos_since_boot();
    crate::irq_ring::isr_publish(IrqSource::Xhci, timestamp);
    // Forces a scheduler entry on IRQ return so drain_irqs polls now, not at the next quantum tick.
    crate::preempt::set_need_resched();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// xHCI MSI-X entry (see `device_irq_entry` for the asm contract).
    pub(super) fn xhci_entry => xhci_handler
}
