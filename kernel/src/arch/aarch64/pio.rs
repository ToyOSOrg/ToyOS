//! The I/O port space: AArch64 has none, and so no ISA function to grant.

use core::convert::Infallible;

use crate::process::Pid;

/// Whether this architecture has an I/O port space at all. Firmware tables
/// that name a port are only honoured where it does.
pub const EXISTS: bool = false;

/// Nothing: every `isa` claim is refused as naming no function.
pub const GRANTABLE: &[crate::isa::Grantable] = &[];

/// No line is routed where no row exists.
pub type Line = Infallible;

/// Never called: [`GRANTABLE`] has no row to route.
pub fn route(_row: usize, _irq: u8) -> Result<Line, alloc::string::String> {
    unreachable!("AArch64 has no ISA bus")
}

pub fn set_masked(line: Line, _masked: bool) {
    match line {}
}

/// Never called: nothing is bound where nothing can be claimed.
pub fn switch_to(_pid: Option<Pid>) {
    unreachable!("AArch64 has no I/O permission bitmap")
}

/// Never called: every caller checks [`EXISTS`] first.
pub unsafe fn outb(_port: u16, _value: u8) {
    unreachable!("AArch64 has no I/O port space")
}

/// Never called: every caller checks [`EXISTS`] first.
pub unsafe fn outw(_port: u16, _value: u16) {
    unreachable!("AArch64 has no I/O port space")
}
