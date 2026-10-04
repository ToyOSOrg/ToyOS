//! The I/O port space as a process reaches it: the ISA functions it can be
//! handed, their lines, and this CPU's I/O permission bitmap, whose decisions
//! are `toyos_userbound::port`'s.

use alloc::format;
use alloc::string::String;

use toyos_userbound::PortAccess;

use super::ioapic::{self, Gsi};
use crate::isa::Grantable;
use crate::process::Pid;

/// The functions a process may be handed: the i8042's data and command ports,
/// its keyboard line and its aux line.
pub const GRANTABLE: &[Grantable] = &[Grantable {
    name: "the i8042",
    ports: &[0x60, 0x64],
    irqs: &[1, 12],
    kernel_drives: super::i8042::drives,
}];

/// A routed line: the I/O APIC input it arrives on.
pub type Line = Gsi;

/// Point ISA line `irq` at the row's vector on the CPU every device interrupt
/// targets, masked.
pub fn route(row: usize, irq: u8) -> Result<Line, String> {
    let line = ioapic::gsi_for_isa_irq(irq).ok_or_else(|| String::from("no I/O APIC"))?;
    ioapic::route(
        line.gsi,
        super::idt::ISA_VECTORS[row],
        crate::drivers::pci::MSG_DEST,
        line.trigger,
        line.polarity,
    )
    .map_err(|why| format!("{why:?}"))?;
    Ok(line.gsi)
}

pub fn set_masked(line: Line, masked: bool) {
    ioapic::set_masked(line, masked).expect("a line `route` placed has a unit");
}

/// Open every row's ports if `pid` holds that row and close them otherwise, on
/// this CPU: at every switch, with `pid` the incoming task's process, under the
/// pass's preemption hold; and at a bind, with interrupts closed.
pub fn switch_to(pid: Option<Pid>) {
    let rows = GRANTABLE
        .iter()
        .enumerate()
        .map(|(row, grantable)| (grantable.ports, pid.is_some_and(|pid| crate::isa::bound_to(row, pid))));
    super::percpu::io_bitmap(|bitmap| bitmap.switch_to(rows));
}

/// The port `access` faulted on, by this CPU's bitmap: the process that
/// faulted is still this CPU's, and its #GP's handler runs with interrupts off.
pub fn refused_port(access: PortAccess) -> Option<u16> {
    super::percpu::io_bitmap(|bitmap| bitmap.refused(access))
}
