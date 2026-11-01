use super::device_irq::device_irq_entry;

extern "sysv64" fn rtc_handler() {
    crate::arch::percpu::irq_took!(Rtc);
    crate::arch::rtc::update_ended();
    crate::arch::apic::eoi();
}

device_irq_entry! {
    /// The RTC's IRQ8 entry (see `device_irq_entry` for the asm contract).
    pub(super) fn rtc_entry => rtc_handler
}
