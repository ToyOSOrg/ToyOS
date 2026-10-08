//! Reset and power-off through the FADT: its reset register, and S5 soft-off
//! through the PM1a control block with the `SLP_TYPa` the holder of the `acpi`
//! claim supplied.
//!
//! **This kernel reads no AML, so it knows no sleep type of its own.** `\_S5`
//! is the firmware's AML to evaluate, and the claim's holder does
//! (`acpi_mode::s5`); until one has, [`off_refused`] says so and a shutdown is
//! refused before anything is stopped. A holder supplies it once, and what
//! it supplied outlives it, until the next claim's holder supplies its own:
//! it is a fact of the machine's tables and not of the process that read
//! them.
//!
//! All input is firmware-supplied and untrusted: a table that does not decode
//! is a machine with no reboot or no PM1a control block, said by name, never
//! a panic.

use core::sync::atomic::{AtomicU8, Ordering};

use toyos_acpi::Reset;
use toyos_userbound::firmware::SleepType;
use toyos_userbound::{Mediated, Ports};

use super::cpu;
use super::pio::{self, Declared, Slot};
use crate::drivers::acpi::direct_phys;
use crate::log;
use crate::time::{Deadline, Duration, Tripwire};

/// PM1 control (ACPI 6.5 Table 4.16): `SCI_EN` and `SLP_EN`.
pub const SCI_EN: u16 = 1 << 0;
const SLP_EN: u16 = 1 << 13;

/// `SCI_EN` is read through it too.
static PM1A_CNT: Slot = Slot::empty();
/// `\_S5`'s `SLP_TYPa` as the `acpi` claim's holder supplied it, or
/// [`UNSUPPLIED`], which is no [`SleepType`].
static SLP_TYPA: AtomicU8 = AtomicU8::new(UNSUPPLIED);
const UNSUPPLIED: u8 = u8::MAX;

static RESET: Slot = Slot::empty();
static RESET_VALUE: AtomicU8 = AtomicU8::new(0);

/// Record the FADT's reset register, or say by name why this machine has none.
///
/// Before `percpu::init_bsp` loads the IDT: from then on every panic can be
/// reported, and a panic that can be reported but not ended is a machine that
/// still needs a hand. Walking these tables inside the panic handler instead is
/// refused — a table walk on a machine that has already failed once is how a
/// panic becomes a triple fault.
pub fn init_reset(rsdp_addr: u64) {
    let fadt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"FACP", toyos_acpi::FADT_FOR_RESET) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no reboot, a panic will hold the panel");
            return;
        }
    };
    match toyos_acpi::reset_register(&fadt) {
        Reset::Port { port, value } => match pio::declare("the reset register", Ports::one(port), Mediated::Kept) {
            Ok(declared) => {
                RESET_VALUE.store(value, Ordering::Relaxed);
                RESET.set(declared);
                log!("ACPI: reset register SystemIO {port:#x} <- {value:#04x}");
            }
            Err(why) => log!("ACPI: reset register {port:#x} not declared ({why:?}) — no reboot"),
        },
        other => log!("ACPI: no reset register this kernel writes ({other:?}) — no reboot"),
    }
}

/// Declare the FADT's PM1a control block, or say by name why this machine
/// has none; it keeps booting either way. After the IDT, so a fault in the
/// table walk is reported.
pub fn init_control(rsdp_addr: u64) {
    let fadt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"FACP", toyos_acpi::FADT_FOR_FIXED_HARDWARE) {
        Ok(table) => table,
        Err(e) => return log!("ACPI: FADT unusable: {e:?} — no PM1a control block, so no ACPI row and no power-off"),
    };
    let block = match toyos_acpi::pm1a_control(&fadt) {
        Ok(block) => block,
        Err(refused) => return log!("ACPI: no PM1a control block this kernel writes ({refused:?}) — no ACPI row and no power-off"),
    };
    let run = Ports::new(block.port, block.len).expect("pm1a_control bounded the block by the port space");
    match pio::declare("the PM1a control block", run, Mediated::ReadOnly) {
        Ok(declared) => PM1A_CNT.set(declared),
        Err(why) => log!("ACPI: PM1a control block {:#x} not declared ({why:?}) — no ACPI row and no power-off", block.port),
    }
}

/// Take `\_S5`'s `SLP_TYPa` from the `acpi` claim's holder, which
/// `acpi_mode::s5` lets supply one: the next claim's holder replaces it, the
/// last to have read the tables being the one believed.
pub fn supply(slp_typ: SleepType) {
    let control = PM1A_CNT.get().expect("an acpi claim exists only over a declared PM1a control block");
    SLP_TYPA.store(slp_typ.get(), Ordering::Release);
    log!("power: S5 is PM1a {:#x} with SLP_TYPa={}, as the acpi claim's holder supplied it", control.ports().first(), slp_typ.get());
}

/// What a holder supplied, if one has.
fn sleep_type() -> Option<SleepType> {
    toyos_userbound::firmware::sleep_type(u64::from(SLP_TYPA.load(Ordering::Acquire)))
}

/// Why this machine has no power-off this kernel performs, where it has none.
pub fn off_refused() -> Option<&'static str> {
    sleep_type().is_none().then_some("no ACPI server supplied S5")
}

pub fn can_reset() -> bool {
    RESET.get().is_some()
}

/// The PM1a control block, where the FADT named one this kernel declared.
pub fn pm1a_control() -> Option<Declared> {
    PM1A_CNT.get()
}

/// Write the reset register and nothing else: no lock, nothing but the port
/// the FADT named. A machine with no reset register halts.
// No fallback: 0xCF9, the keyboard controller and anything else are written only where a table named them.
pub fn reset() -> ! {
    if let Some(reset) = RESET.get() {
        // SAFETY: the port `init_reset` decoded as an 8-bit System I/O register and declared, and the value is that register's.
        unsafe { cpu::outb(reset.port(0), RESET_VALUE.load(Ordering::Relaxed)) };
    }
    cpu::halt()
}

/// How long a machine may go on running once `SLP_EN` is written.
const S5_TAKES: Tripwire = Tripwire::absurd(
    Duration::from_secs(2),
    "a platform sequences S5 in milliseconds once SLP_EN is written, so one still running \
     this kernel two seconds later never entered it",
);

/// Enter S5. The caller asked [`off_refused`] before it stopped the machine:
/// no sleep type here is this kernel's defect, and a panic.
///
/// ACPI 6.5 §16.1.6's order: on a machine in ACPI mode, which is the OS's
/// to put to sleep, every event is disabled and every status cleared first
/// (`acpi_mode::quiet`), so no event pending at the write wakes it again;
/// then `SLP_TYP`, and then `SLP_TYP` with `SLP_EN`, every other bit of the
/// register as it reads.
///
/// **A write the platform does not act on is a panic past [`S5_TAKES`]**,
/// naming what the registers and this CPU's SMI count read either side of it:
/// the panel shows it, and the black box carries it through the panic's reset,
/// where a halt would leave a machine that is on, silent, and indistinguishable
/// from one the power left.
pub fn off(stopping: crate::quiesce::Stopping) -> ! {
    let taken = pio::take_back(&stopping);
    super::smi_cmd::settle(&taken);
    super::acpi_mode::settle(&taken);
    // Read once no holder's call is in flight: the one a supply logged last.
    let slp_typ = sleep_type().expect("power: the machine was stopped for a power-off with no SLP_TYPa: the acpi claim's holder supplies it, and a shutdown without one is refused before the stop");
    let control = PM1A_CNT.get().expect("power: a sleep type was supplied over no PM1a control block").port(0);
    let held = cpu::inw(control);
    if held & SCI_EN != 0 {
        super::acpi_mode::quiet(&taken);
    }
    let typed = slp_typ.in_control(held & !SLP_EN);
    let smis_before = super::counters::read().smi;
    // SAFETY: the block `init_control` declared, and a `SLP_TYPa` the field holds: what it does is the firmware's, as its `\_S5` names it.
    unsafe {
        cpu::outw(control, typed);
        cpu::outw(control, typed | SLP_EN);
    }
    let by = Deadline::at(crate::clock::now() + Duration::from_nanos(S5_TAKES.nanos()));
    while !by.reached(crate::clock::now()) {
        core::hint::spin_loop();
    }
    let now = cpu::inw(control);
    panic!(
        "power: S5 did not take: the machine still runs {S5_TAKES} after SLP_EN; PM1a_CNT read {held:#06x} before \
         the write of {:#06x} and reads {now:#06x} now, SCI_EN {}; {}; cpu{}'s SMI count {} before the write and {} now",
        typed | SLP_EN,
        if now & SCI_EN == 0 { "clear" } else { "set" },
        super::acpi_mode::pm1_events(&taken),
        super::percpu::cpu_id(),
        smis_before.map_or_else(|| "unread".into(), |n| alloc::format!("{n}")),
        super::counters::read().smi.map_or_else(|| "unread".into(), |n| alloc::format!("{n}")),
    )
}
