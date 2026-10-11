use super::device_irq::device_irq_entry;

extern "sysv64" fn kick_handler() {
    let t0 = crate::counters::soft::now();
    crate::arch::percpu::irq_took!(Kick);
    crate::counters::serve_here();
    crate::arch::smi_cmd::serve_here();
    crate::preempt::set_need_resched();
    crate::arch::apic::eoi();
    crate::counters::soft::since(toyos_abi::counters::Counter::IrqCycles, t0);
}

device_irq_entry! {
    /// `apic::kick_cpu`'s IPI entry (see `device_irq_entry` for the asm contract).
    pub(super) fn kick_entry => kick_handler
}
