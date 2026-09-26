//! The I/O port space: AArch64 has none.

/// Whether this architecture has an I/O port space at all. Firmware tables
/// that name a port are only honoured where it does.
pub const EXISTS: bool = false;

/// Never called: every caller checks [`EXISTS`] first.
pub unsafe fn outb(_port: u16, _value: u8) {
    unreachable!("AArch64 has no I/O port space")
}

/// Never called: every caller checks [`EXISTS`] first.
pub unsafe fn outw(_port: u16, _value: u16) {
    unreachable!("AArch64 has no I/O port space")
}
