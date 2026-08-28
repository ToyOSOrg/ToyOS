use super::device_irq::device_irq_entry;

// Reports the IOMMU blocking a device, not device work finished, so it skips irq_ring and need_resched.
extern "sysv64" fn dma_fault_handler() {
    crate::irq_census::irq_took!(DmaFault);
    crate::iommu::fault_interrupt();
}

device_irq_entry! {
    /// DMA remapping fault entry (see `device_irq_entry` for the asm contract).
    pub(super) fn dma_fault_entry => dma_fault_handler
}
