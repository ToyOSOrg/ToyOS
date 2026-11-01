//! The I/O port space: AArch64 has none, so no port to declare or open, and
//! no line a row raises; the `acpi` claim's row names neither.

use core::convert::Infallible;

use toyos_userbound::Ports;

use crate::process::Pid;

/// No row raises a line, so no wire is resolved.
pub type Wire = Infallible;

/// No line is routed where no row raises one.
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

/// Nothing: there is no I/O permission bitmap, and a row's binding opens no
/// port.
pub fn switch_to(_pid: Option<Pid>) {}
