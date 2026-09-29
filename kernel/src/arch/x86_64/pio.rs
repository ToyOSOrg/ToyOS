//! The I/O port space: x86-64 has one, reached by `in` and `out`.
//!
//! **A process reaches a port only through its CPU's I/O permission bitmap,
//! with IOPL left at 0** (Intel SDM Vol. 1 §19.5.2, AMD APM Vol. 2 §12.2.4):
//! each CPU's TSS carries one bit per port below [`IO_PORTS`], set unless the
//! process running there holds a [`GRANTABLE`] row naming that port, and
//! every port at or past it is past the TSS limit, which the processor refuses
//! by itself.

use alloc::format;
use alloc::string::String;

use super::ioapic::{self, Gsi};
pub use super::percpu::IO_PORTS;
use crate::isa::Grantable;
use crate::process::Pid;

/// Whether this architecture has an I/O port space at all. Firmware tables
/// that name a port are only honoured where it does.
pub const EXISTS: bool = true;

pub use super::cpu::{outb, outw};

/// The functions a process may be handed: the i8042's data and command ports,
/// its keyboard line and its aux line.
pub const GRANTABLE: &[Grantable] = &[Grantable {
    name: "the i8042",
    ports: &[0x60, 0x64],
    irqs: &[1, 12],
    kernel_drives: super::i8042::drives,
}];

const _: () = {
    let mut row = 0;
    while row < GRANTABLE.len() {
        let mut i = 0;
        while i < GRANTABLE[row].ports.len() {
            assert!((GRANTABLE[row].ports[i] as usize) < IO_PORTS, "a port past the bitmap");
            i += 1;
        }
        row += 1;
    }
};

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
/// this CPU: at every switch, with `pid` the incoming task's process.
pub fn switch_to(pid: Option<Pid>) {
    for (row, grantable) in GRANTABLE.iter().enumerate() {
        let open = pid.is_some_and(|pid| crate::isa::bound_to(row, pid));
        // A row's ports open and close together, so its first bit is its state.
        let Some(&first) = grantable.ports.first() else { continue };
        if super::percpu::port_open(first) == open {
            continue;
        }
        for &port in grantable.ports {
            super::percpu::set_port_open(port, open);
        }
    }
}

/// The first port of `access` this CPU refuses Ring 3, which is the port its
/// #GP faulted on; the process that faulted is still this CPU's. `None` if
/// every port in the span is open: the decode named a port the bitmap does
/// not actually refuse, and the kill record must not guess one.
pub fn refused_port(access: PortAccess) -> Option<u16> {
    (0..u16::from(access.bytes))
        .map(|i| access.port.wrapping_add(i))
        .find(|&port| !super::percpu::port_open(port))
}

/// An `in` or `out` as decoded from the bytes at a faulting instruction.
#[derive(Clone, Copy)]
pub struct PortAccess {
    pub out: bool,
    pub port: u16,
    /// How many ports from `port` on the access spans, every one of which the
    /// bitmap must open.
    pub bytes: u8,
}

impl core::fmt::Display for PortAccess {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (verb, way) = if self.out { ("out", "to") } else { ("in", "from") };
        write!(f, "{verb} of {} byte(s) {way} port {:#06x}", self.bytes, self.port)
    }
}

/// The port access `code` begins with, or `None` for any other instruction.
/// What a Ring 3 #GP names, since its error code (0) says nothing; `dx` is
/// what the forms that take their port from DX read.
///
/// Up to two prefixes of those an `in`/`out` can carry: operand size (which
/// halves a 4-byte access), a REP for the string forms, and REX.
pub const fn port_access(code: [u8; 4], dx: u16) -> Option<PortAccess> {
    const fn prefix(byte: u8) -> bool {
        matches!(byte, 0x66 | 0xF2 | 0xF3 | 0x40..=0x4F)
    }
    // Destructured rather than indexed: the crash report calls this, and
    // nothing on that path may panic.
    let (half, op, imm) = match code {
        [a, b, op, imm] if prefix(a) && prefix(b) => (a == 0x66 || b == 0x66, op, imm),
        [a, op, imm, _] if prefix(a) => (a == 0x66, op, imm),
        [op, imm, _, _] => (false, op, imm),
    };
    let wide = if half { 2 } else { 4 };
    let (out, port, bytes) = match op {
        0xE4 => (false, imm as u16, 1),
        0xE5 => (false, imm as u16, wide),
        0xE6 => (true, imm as u16, 1),
        0xE7 => (true, imm as u16, wide),
        0xEC | 0x6C => (false, dx, 1),
        0xED | 0x6D => (false, dx, wide),
        0xEE | 0x6E => (true, dx, 1),
        0xEF | 0x6F => (true, dx, wide),
        _ => return None,
    };
    Some(PortAccess { out, port, bytes })
}

const _: () = {
    const fn is(access: Option<PortAccess>, out: bool, port: u16, bytes: u8) -> bool {
        match access {
            Some(a) => a.out == out && a.port == port && a.bytes == bytes,
            None => false,
        }
    }
    // `in al, 0x61`, `out 0x64, al`, `in eax, dx`, `in ax, dx` and `rep outsb`.
    assert!(is(port_access([0xE4, 0x61, 0, 0], 0), false, 0x61, 1));
    assert!(is(port_access([0xE6, 0x64, 0, 0], 0), true, 0x64, 1));
    assert!(is(port_access([0xED, 0, 0, 0], 0x60), false, 0x60, 4));
    assert!(is(port_access([0x66, 0xED, 0, 0], 0x60), false, 0x60, 2));
    assert!(is(port_access([0xF3, 0x6E, 0, 0], 0x3F8), true, 0x3F8, 1));
    // `mov eax, 0x61` (B8) and `hlt` are no port access.
    assert!(port_access([0xB8, 0x61, 0, 0], 0).is_none());
    assert!(port_access([0xF4, 0, 0, 0], 0).is_none());
};
