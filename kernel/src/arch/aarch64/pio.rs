//! The I/O port space: AArch64 has none, and so no row to fill and no port to
//! declare.

use core::convert::Infallible;

use toyos_userbound::Ports;

use crate::process::Pid;

/// No row is filled, so no wire is resolved.
pub type Wire = Infallible;

/// No line is routed where no row exists.
pub type Line = Infallible;

pub fn route(_row: usize, wire: Wire) -> Result<Line, alloc::string::String> {
    match wire {}
}

pub fn level(line: Line) -> bool {
    match line {}
}

pub fn describe(wire: Wire) -> alloc::string::String {
    match wire {}
}

pub fn same(a: Wire, _b: Wire) -> bool {
    match a {}
}

pub fn set_masked(line: Line, _masked: bool) {
    match line {}
}

/// Nothing is declared where there are no ports.
pub fn holder(_ports: Ports) -> Option<&'static str> {
    None
}

/// Never called: nothing is bound where nothing can be claimed.
pub fn switch_to(_pid: Option<Pid>) {
    unreachable!("AArch64 has no I/O permission bitmap")
}
