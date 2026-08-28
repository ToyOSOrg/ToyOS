use super::device_irq::device_irq_entry;

device_irq_entry! {
    /// The kernel's only reader of port 0x60 for both PS/2 lines.
    pub(super) fn i8042_entry => crate::drivers::i8042::handler
}
