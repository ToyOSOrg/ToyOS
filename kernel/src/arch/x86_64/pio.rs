//! The I/O port space: the one declaration of every port this kernel drives,
//! the only maker of the [`Port`] every `in` and `out` it makes takes; the
//! lines a granted function raises; and this CPU's I/O permission bitmap,
//! whose decisions are `toyos_userbound::port`'s.
//!
//! **A port the kernel drives is declared before it is touched, and no grant
//! reaches a declared port**: [`FIXED`] holds what every machine has, and
//! [`declare`] what a probe or a firmware table names at boot, each refused
//! where it shares a port with a run declared before it. `crate::isa` asks
//! [`holder`] before it opens a row.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_userbound::{PortAccess, Ports, Reserved, Undeclared};

use super::ioapic::{self, Gsi, IsaLine, Trigger};
use crate::log;
use crate::process::Pid;
use crate::sync::Lock;

/// One port, declared: the argument of every `in` and `out` this kernel makes.
#[derive(Clone, Copy)]
pub struct Port(u16);

impl Port {
    pub(super) const fn number(self) -> u16 {
        self.0
    }
}

/// A run of ports declared to one holder: the only maker of a [`Port`].
#[derive(Clone, Copy)]
pub struct Declared(Ports);

impl Declared {
    const fn fixed(first: u16, count: u16) -> Self {
        match Ports::new(first, count) {
            Some(ports) => Self(ports),
            None => panic!("a fixed run inside the port space"),
        }
    }

    /// The port `offset` into the run; past its end is a kernel bug.
    pub const fn port(self, offset: u16) -> Port {
        match self.0.at(offset) {
            Some(port) => Port(port),
            None => panic!("pio: an offset past the declared run"),
        }
    }

    pub const fn ports(self) -> Ports {
        self.0
    }
}

pub const COM1: Declared = Declared::fixed(0x3F8, 8);
pub const PIC_PRIMARY: Declared = Declared::fixed(0x20, 2);
pub const PIC_SECONDARY: Declared = Declared::fixed(0xA0, 2);
/// POST codes, which nothing decodes: one bus cycle of delay.
pub const POST: Declared = Declared::fixed(0x80, 1);
pub const CMOS: Declared = Declared::fixed(0x70, 2);

/// The runs every machine has, and their holders. The PCI configuration
/// mechanism is here though this kernel reaches configuration space through
/// ECAM: it is the kernel's all the same, and a grant of it would be a way
/// round every claim. `CONFIG_ADDRESS` is a dword at 0xCF8, so its first port
/// alone keeps it refused, and 0xCF9 free for a reset register.
const FIXED: &[(&str, Declared)] = &[
    ("COM1", COM1),
    ("the 8259 pair", PIC_PRIMARY),
    ("the 8259 pair", PIC_SECONDARY),
    ("the POST port", POST),
    ("the CMOS RTC", CMOS),
    ("the PCI configuration mechanism", Declared::fixed(0xCF8, 1)),
    ("the PCI configuration mechanism", Declared::fixed(0xCFC, 4)),
];

const _: () = {
    let mut i = 0;
    while i < FIXED.len() {
        let mut j = i + 1;
        while j < FIXED.len() {
            assert!(!FIXED[i].1.0.overlaps(FIXED[j].1.0), "two fixed runs share a port");
            j += 1;
        }
        i += 1;
    }
};

/// What probes and firmware tables declared at boot.
static RUNTIME: Lock<Reserved<8>> = Lock::new(Reserved::new());

/// Declare `ports` to `holder`, refused where another holder has one of them.
/// Boot's alone: a run declared once a process could hold a grant would be a
/// port the grant was not checked against.
pub fn declare(holder: &'static str, ports: Ports) -> Result<Declared, Undeclared> {
    assert!(!crate::smp::is_ready(), "pio: {holder} declared ports after userland could hold a grant");
    if let Some(&(first, _)) = FIXED.iter().find(|(_, fixed)| fixed.0.overlaps(ports)) {
        return Err(Undeclared::Clash(first));
    }
    RUNTIME.lock().declare(holder, ports)?;
    Ok(Declared(ports))
}

/// Who holds a port of `ports`, if this kernel declared one.
pub fn holder(ports: Ports) -> Option<&'static str> {
    FIXED.iter().find(|(_, fixed)| fixed.0.overlaps(ports)).map(|&(name, _)| name).or_else(|| RUNTIME.lock().holder(ports))
}

/// Every row's ports, the kernel's again: the power-off's, and nobody else's.
pub struct TakenBack(());

/// Take every row's ports back from whoever holds them, whatever the stop's
/// record says. Once `stopping` exists a thread enters Ring 3 only past
/// `scheduler::leave_user_if_due`, which stops all but the stop's caller; the
/// shootdown returns only once every other CPU has answered it from Ring 0,
/// so no thread that was in Ring 3 before is there still.
pub fn take_back(_stopping: &crate::quiesce::Stopping) -> TakenBack {
    super::tlb::shootdown(crate::invalidation::Origin::Stop);
    TakenBack(())
}

impl TakenBack {
    /// The `at`th of the runs `row` was filled with.
    pub fn run(&self, row: usize, at: usize) -> Declared {
        Declared(crate::isa::runs(row).expect("pio: a row the boot never filled, taken back")[at])
    }
}

/// A [`Declared`] kept where a later reader finds it; empty until set.
pub struct Slot(AtomicU32);

impl Slot {
    pub const fn empty() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn set(&self, declared: Declared) {
        let bits = u32::from(declared.0.first()) | u32::from(declared.0.count()) << 16;
        let was = self.0.swap(bits, Ordering::Release);
        assert!(was == 0, "pio: a slot set twice");
    }

    pub fn get(&self) -> Option<Declared> {
        let bits = self.0.load(Ordering::Acquire);
        // A run is never empty, so a set slot is never 0.
        (bits != 0).then(|| Declared(Ports::new(bits as u16, (bits >> 16) as u16).expect("a run `set` stored")))
    }
}

/// A line a row's function raises, resolved against the MADT.
pub type Wire = IsaLine;

/// A routed line: the I/O APIC input it arrives on, and whether it is level.
#[derive(Clone, Copy)]
pub struct Line {
    gsi: Gsi,
    level: bool,
}

/// Point `wire` at the row's vector on the CPU every device interrupt
/// targets, masked.
pub fn route(row: usize, wire: Wire) -> Result<Line, String> {
    ioapic::route(
        wire.gsi,
        super::idt::ISA_VECTORS[row],
        crate::drivers::pci::MSG_DEST,
        wire.trigger,
        wire.polarity,
    )
    .map_err(|why| format!("{why:?}"))?;
    Ok(Line { gsi: wire.gsi, level: wire.trigger == Trigger::Level })
}

/// Whether the line stays asserted until its source is served: masked by its
/// handler, unmasked by its holder's acknowledgement.
pub fn level(line: Line) -> bool {
    line.level
}

/// The same line, by its GSI: a second wire on it would share it.
pub fn same(a: Wire, b: Wire) -> bool {
    a.gsi == b.gsi
}

pub fn describe(wire: Wire) -> String {
    format!("gsi {} {}", wire.gsi.0, ioapic::describe(wire.trigger, wire.polarity))
}

/// Callable from the line's own handler.
pub fn set_masked(line: Line, masked: bool) {
    ioapic::set_masked(line.gsi, masked).expect("a line `route` placed has a unit");
}

/// Open every row's ports if `pid` holds that row and close them otherwise, on
/// this CPU: at every switch, with `pid` the incoming task's process, under the
/// pass's preemption hold; and at a bind, with interrupts closed.
pub fn switch_to(pid: Option<Pid>) {
    let rows = (0..crate::isa::MAX_ROWS)
        .filter_map(|row| crate::isa::runs(row).map(|runs| (runs, pid.is_some_and(|pid| crate::isa::bound_to(row, pid)))));
    super::percpu::io_bitmap(|bitmap| bitmap.switch_to(rows));
}

/// The port `access` faulted on, by this CPU's bitmap: the process that
/// faulted is still this CPU's, and its #GP's handler runs with interrupts off.
pub fn refused_port(access: PortAccess) -> Option<u16> {
    super::percpu::io_bitmap(|bitmap| bitmap.refused(access))
}

/// `isa`'s row for the i8042.
pub const I8042_ROW: usize = 0;

/// Fill the i8042's row: its data and command ports, its keyboard line and
/// its aux line, as `isa:0060,0064:1,12` names them.
pub fn fill_i8042_row() {
    let wires: Option<alloc::vec::Vec<Wire>> = [1, 12].into_iter().map(ioapic::gsi_for_isa_irq).collect();
    let Some(wires) = wires else { return log!("isa: no i8042 row — no I/O APIC carries its lines") };
    crate::isa::fill(
        I8042_ROW,
        crate::isa::Function {
            name: "the i8042",
            runs: alloc::vec![Ports::one(0x60), Ports::one(0x64)],
            irqs: alloc::vec![1, 12],
            wires,
        },
    );
}
