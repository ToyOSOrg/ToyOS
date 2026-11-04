//! The I/O port space, which AArch64 has none of, and the lines a row's
//! function raises: an SPI each, routed to the boot CPU and taken by
//! `crate::isa`'s handler for the row ([`row_of`]).

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

use toyos_userbound::Ports;

use crate::process::Pid;

/// A line a row's function raises: an SPI, and whether it is level-triggered.
#[derive(Clone, Copy)]
pub struct Wire {
    pub intid: u32,
    pub level: bool,
}

/// A routed line.
pub type Line = Wire;

/// Each row's SPI once routed, zero until then: what the interrupt's handler
/// finds its row by.
static ROUTED: [AtomicU32; crate::isa::MAX_ROWS] = [const { AtomicU32::new(0) }; crate::isa::MAX_ROWS];

/// Route `wire` to the boot CPU for `row`, masked.
pub fn route(row: usize, wire: Wire) -> Result<Line, alloc::string::String> {
    super::irqchip::route_spi(wire.intid, wire.level)
        .map_err(|limit| alloc::format!("the distributor takes no SPI {} (its limit is {limit})", wire.intid))?;
    ROUTED[row].store(wire.intid, Relaxed);
    Ok(wire)
}

/// The row whose routed line `intid` is.
pub fn row_of(intid: u32) -> Option<usize> {
    (intid != 0).then(|| ROUTED.iter().position(|routed| routed.load(Relaxed) == intid)).flatten()
}

pub fn level(line: Line) -> bool {
    line.level
}

pub fn describe(wire: Wire) -> alloc::string::String {
    alloc::format!("SPI {} {}", wire.intid, if wire.level { "level" } else { "edge" })
}

pub fn same(a: Wire, b: Wire) -> bool {
    a.intid == b.intid
}

/// Callable from the line's own handler.
pub fn set_masked(line: Line, masked: bool) {
    super::irqchip::set_spi_masked(line.intid, masked);
}

/// Nothing is declared where there are no ports.
pub fn holder(_ports: Ports) -> Option<&'static str> {
    None
}

/// Nothing: there is no I/O permission bitmap, and a row's binding opens no
/// port.
pub fn switch_to(_pid: Option<Pid>) {}
