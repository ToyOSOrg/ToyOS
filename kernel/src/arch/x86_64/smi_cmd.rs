//! `SMI_CMD`: the port a write to is a command to the firmware, and the one
//! function that writes it.
//!
//! **Every write is made on the boot processor, wherever its caller runs.**
//! ACPI 6.5 Table 5.9, of `SMI_CMD`: "OSPM issues commands to the SMI_CMD port
//! synchronously from the boot processor"; and of `ACPI_DISABLE`, "writing
//! ACPI_DISABLE to the SMI_CMD port from the boot processor". The boot
//! processor is cpu0: the CPU the firmware handed over on, which brought up
//! every other.
//!
//! A caller on another CPU asks a round of [`Shootdown`]'s protocol with the
//! boot processor as its one target, as a counters read asks one of every CPU:
//! it kicks the boot processor, which makes the write from its kick handler
//! whether it was idle or busy, or from a lock's spin where its interrupts are
//! closed ([`serve_here`]), and spins until that round is answered. [`write`]
//! therefore returns only once the `out` has retired, which is after the
//! firmware's handler has. A boot processor that answers neither way within
//! [`DEAF_CPU`] is a panic, as one that answers no TLB shootdown is.
//!
//! The port is this module's alone, so the `out` in [`answer`] is the only
//! one the kernel can make to it, and [`answer`] reads which CPU it is on
//! with interrupts closed beside that `out`: no caller's state decides it.
//! It is counted there too, with the time it held the boot processor
//! ([`counted`]), whoever asked for it.
//!
//! **The `acpi` claim's holder writes no byte here: it asks for one**, and the
//! declaration says which bytes are never written for it
//! (`toyos_userbound::Mediated::Command`): those the FADT gives a meaning,
//! which are this kernel's own commands.
//!
//! **None is made once the stop has begun**: the power-off waits out a write
//! in flight ([`settle`]) and then owns the hardware, and an SMI it did not
//! make is one its S5 entry was never measured against.

use core::fmt;
use alloc::string::String;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};

use toyos_userbound::{KeptCommands, Mediated, Ports, Undeclared};

use super::pio::{self, Slot, TakenBack};
use super::{apic, cpu, percpu, IrqGuard};
use crate::shootdown::Shootdown;
use crate::sync::Lock;
use crate::time::{Deadline, Duration, DEAF_CPU};

/// The boot processor.
const BOOT: u32 = 0;

/// The port, where the FADT names one.
static PORT: Slot = Slot::empty();

/// Held from a write's ask to its answer, so one round is in flight at most
/// and each is answered by one write.
static WRITING: Lock<()> = Lock::new(());
static ROUND: Shootdown = Shootdown::new();
/// What the round in flight writes: stored before the round is issued, read by its answer.
static ASKED: AtomicU8 = AtomicU8::new(0);
/// What the last answer read, published by the round's own answer.
static ON: AtomicU32 = AtomicU32::new(0);
static HELD_NS: AtomicU64 = AtomicU64::new(0);
/// Every write made and the nanoseconds they held the boot processor, which
/// alone writes and reads them.
static WRITES: AtomicU64 = AtomicU64::new(0);
static SPENT_NS: AtomicU64 = AtomicU64::new(0);
static SMIS: [AtomicU64; 2] = [const { AtomicU64::new(UNREAD) }; 2];

/// No SMI count: the register holds 32 bits.
const UNREAD: u64 = u64::MAX;

/// One write, as the CPU that made it read it.
pub struct Written {
    /// The CPU the `out` ran on, read there.
    on: u32,
    asked_from: u32,
    /// From before the `out` to after it, which is the firmware's handler
    /// where the write raises an SMI.
    held: Duration,
    /// That CPU's SMI count either side of the write, where it reads one.
    smis: [u64; 2],
}

impl Written {
    pub fn held(&self) -> Duration {
        self.held
    }
}

impl fmt::Display for Written {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { on, asked_from, held, smis } = self;
        let [before, after] = smis.map(|n| if n == UNREAD { String::from("unread") } else { alloc::format!("{n}") });
        write!(
            f,
            "on cpu{on}, asked from cpu{asked_from}; the write held cpu{on} {}ns, its SMI count {before} before the write and {after} after",
            held.nanos()
        )
    }
}

/// Declare the port the FADT names, and the values it names for it. Boot's.
/// The `acpi` claim's holder reads it and never writes it: a write is a
/// command to the firmware, and [`write`] makes every one.
pub fn declare(port: u16, named: [u8; 5]) -> Result<(), Undeclared> {
    PORT.set(pio::declare("SMI_CMD", Ports::one(port), Mediated::Command(KeptCommands(named)))?);
    Ok(())
}

/// How many writes this CPU has made and the nanoseconds they held it, on
/// the boot processor of a machine that names the port; `None` on every
/// other CPU, which makes none.
pub fn counted() -> Option<(u64, u64)> {
    (percpu::cpu_id() == BOOT && PORT.get().is_some()).then(|| (WRITES.load(Relaxed), SPENT_NS.load(Relaxed)))
}

/// Write `value` to `SMI_CMD` on the boot processor and return once it is
/// written; `None`, and nothing written, once the stop has begun.
pub fn write(value: u8) -> Option<Written> {
    // Preemption is off under the lock, so this CPU is the caller's throughout.
    let _writing = WRITING.lock();
    if crate::quiesce::begun() {
        return None;
    }
    let asked_from = percpu::cpu_id();
    ASKED.store(value, Relaxed);
    let generation = ROUND.issue();
    // The boot processor's own ask, unless a kick taken since the issue has answered it already.
    answer();
    if !ROUND.served(BOOT as usize, generation) {
        apic::kick_cpu(BOOT);
        let by = Deadline::at(crate::clock::now() + Duration::from_nanos(DEAF_CPU.nanos()));
        while !ROUND.served(BOOT as usize, generation) {
            assert!(
                !by.reached(crate::clock::now()),
                "smi_cmd: the boot processor has not written {value:#04x} for cpu{asked_from} in {DEAF_CPU}: it is not taking interrupts"
            );
            core::hint::spin_loop();
            // A caller with interrupts closed owes the boot processor's shootdown an answer while it waits.
            super::tlb::poll();
        }
    }
    Some(Written {
        on: ON.load(Relaxed),
        asked_from,
        held: Duration::from_nanos(HELD_NS.load(Relaxed)),
        smis: [SMIS[0].load(Relaxed), SMIS[1].load(Relaxed)],
    })
}

/// The boot processor's answer to a round it owes: its kick handler's, and
/// `tlb::poll`'s for a boot processor spinning on a lock with interrupts
/// closed, which takes no kick and may be waiting on the caller's lock.
/// Every CPU calls it; [`answer`] holds which one writes.
#[inline]
pub fn serve_here() {
    // The hint before the mask: with no round in flight a kick or a spin pays two loads.
    if ROUND.owes(BOOT as usize) {
        answer();
    }
}

/// The write the round in flight asks for, if none has answered it and this
/// is the boot processor; nothing on any other CPU. Interrupts are closed
/// from the read of the CPU to the `out`, so the two are one CPU's and no
/// kick's answer nests in this one and writes twice. Takes no lock and
/// allocates nothing.
fn answer() {
    let _closed = IrqGuard::close();
    if percpu::cpu_id() != BOOT {
        return;
    }
    ROUND.serve_if_owed(BOOT as usize, || {
        let port = PORT.get().expect("smi_cmd: a write asked of a machine whose FADT names no SMI_CMD").port(0);
        let value = ASKED.load(Relaxed);
        let smi = || super::counters::read().smi.unwrap_or(UNREAD);
        let before = smi();
        let from = crate::clock::now();
        // SAFETY: `SMI_CMD`, declared; the value is one the FADT names for it
        // or one the mediation's policy passed, by `write`'s caller.
        unsafe { cpu::outb(port, value) };
        let held = (crate::clock::now() - from).nanos();
        HELD_NS.store(held, Relaxed);
        // One writer, this CPU with interrupts closed: no update is lost.
        WRITES.store(WRITES.load(Relaxed) + 1, Relaxed);
        SPENT_NS.store(SPENT_NS.load(Relaxed) + held, Relaxed);
        SMIS[0].store(before, Relaxed);
        SMIS[1].store(smi(), Relaxed);
        ON.store(percpu::cpu_id(), Relaxed);
    });
}

/// Wait out a write to `SMI_CMD` in flight: the stop has begun, so none follows.
pub fn settle(_taken: &TakenBack) {
    drop(WRITING.lock());
}
