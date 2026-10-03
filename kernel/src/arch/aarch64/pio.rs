//! The I/O port space: AArch64 has none, and so no ISA function to grant.

use core::convert::Infallible;

use crate::process::Pid;

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

/// No quarantine to stage a claim against.
#[cfg(feature = "boot-actuators")]
pub fn straddling<R>(claim: impl FnOnce() -> R) -> R {
    claim()
}
