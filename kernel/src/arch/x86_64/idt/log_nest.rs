
use super::device_irq::device_irq_entry;

extern "sysv64" fn log_nest_handler() {
    crate::log::nested::deliver();
    crate::arch::apic::eoi();
}

// Reuses `device_irq_entry!`'s stub: it saves scratch registers and aligns the stack for entry from either ring.
device_irq_entry! {
    /// The self-IPI `log::nested` sends from inside `emit`.
    pub(super) fn log_nest_entry => log_nest_handler
}
