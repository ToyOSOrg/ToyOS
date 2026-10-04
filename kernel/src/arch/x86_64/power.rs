//! Reset and power-off through the FADT: its reset register, and S5 soft-off
//! through the PM1a control block with the `SLP_TYPa` the DSDT's `\_S5_`
//! package names.
//!
//! All input is firmware-supplied and untrusted: a table that does not decode
//! is a machine with no reboot or no soft-off, said by name, never a panic.

use core::mem::size_of;
use core::sync::atomic::{AtomicU8, Ordering};

use toyos_acpi::{Reset, Table, TableError, S5, SDT_HEADER_LEN, SDT_REVISION};

use toyos_userbound::Ports;

use super::cpu;
use super::pio::{self, Declared, Slot};
use crate::drivers::acpi::direct_phys;
use crate::log;

/// PM1 control (ACPI 6.5 Table 4.16): `SCI_EN`, `SLP_TYP` and `SLP_EN`.
pub const SCI_EN: u16 = 1 << 0;
const SLP_TYP: u16 = 0b111 << 10;
const SLP_EN: u16 = 1 << 13;

/// Declared whether or not soft-off decodes: `SCI_EN` is read through it too.
static PM1A_CNT: Slot = Slot::empty();
/// `\_S5_`'s `SLP_TYPa`, shifted into place; 0 until the DSDT named one.
static SLP_TYPA: AtomicU8 = AtomicU8::new(0);
static SOFT_OFF: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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
        Reset::Port { port, value } => match pio::declare("the reset register", Ports::one(port)) {
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

/// Record S5 soft-off, or say by name why this machine has none; it keeps
/// booting either way. After the IDT, so a fault in the DSDT walk is reported.
pub fn init_off(rsdp_addr: u64) {
    const FADT_FOR_POWER: usize = toyos_acpi::FADT_PM1A_CNT_BLK + size_of::<u32>();
    const FADT_FOR_X_DSDT: usize = toyos_acpi::FADT_X_DSDT + size_of::<u64>();

    let fadt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"FACP", FADT_FOR_POWER) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no soft-off, shutdown will halt instead");
            return;
        }
    };

    let Some(block) = fadt.u32_at(toyos_acpi::FADT_PM1A_CNT_BLK).filter(|&block| block != 0) else {
        log!("ACPI: FADT has no PM1a control block — no soft-off");
        return;
    };
    let Some(run) = u16::try_from(block).ok().and_then(|pm1a| Ports::new(pm1a, 2)) else {
        log!("ACPI: FADT puts the PM1a control block at {block:#x}, past the 16-bit port space — no soft-off");
        return;
    };
    let pm1a = run.first();
    match pio::declare("the PM1a control block", run) {
        Ok(declared) => PM1A_CNT.set(declared),
        Err(why) => {
            log!("ACPI: PM1a control block {pm1a:#x} not declared ({why:?}) — no soft-off");
            return;
        }
    }

    // Prefer X_DSDT over DSDT; a revision claiming 2.0 doesn't prove the field is present, so the length is checked rather than trusting the revision alone.
    let dsdt_addr = toyos_acpi::dsdt_address(&fadt);
    if dsdt_addr == 0 {
        log!(
            "ACPI: FADT names no DSDT (rev {:?}, needs {FADT_FOR_X_DSDT} bytes for X_DSDT) — no soft-off",
            fadt.byte(SDT_REVISION)
        );
        return;
    }

    let dsdt = match Table::open(direct_phys(), dsdt_addr, b"DSDT", SDT_HEADER_LEN) {
        Ok(table) => table,
        Err(TableError::Unmapped { .. }) => {
            log!("ACPI: FADT points the DSDT at {dsdt_addr:#x}, which is not an address — no soft-off");
            return;
        }
        Err(e) => {
            log!("ACPI: DSDT at {dsdt_addr:#x} unusable: {e:?} — no soft-off");
            return;
        }
    };

    let slp_typ = match toyos_acpi::s5_slp_typ(&dsdt) {
        S5::SlpTyp(slp_typ) => slp_typ,
        S5::Absent => {
            log!("ACPI: no \\_S5_ package in the DSDT — no soft-off");
            return;
        }
        S5::Wide(byte) => {
            log!("ACPI: the DSDT's \\_S5_ names SLP_TYPa {byte:#x}, wider than its three bits — no soft-off");
            return;
        }
    };

    SLP_TYPA.store(slp_typ, Ordering::Relaxed);
    SOFT_OFF.store(true, Ordering::Release);
    log!("ACPI: PM1a={pm1a:#x} SLP_TYPa={slp_typ}");
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

/// Enter S5, or halt on a machine whose tables named no soft-off.
///
/// ACPI 6.5 §16.1.6's order: on a machine in ACPI mode, which is the OS's
/// to put to sleep, every event is disabled and every status cleared first
/// (`acpi_mode::quiet`), so no event pending at the write wakes it again;
/// then `SLP_TYP`, and then `SLP_TYP` with `SLP_EN`, every other bit of the
/// register as it reads.
pub fn off() -> ! {
    if let (Some(control), true) = (PM1A_CNT.get(), SOFT_OFF.load(Ordering::Acquire)) {
        let control = control.port(0);
        let held = cpu::inw(control);
        if held & SCI_EN != 0 {
            super::acpi_mode::quiet();
        }
        let typed = held & !(SLP_TYP | SLP_EN) | u16::from(SLP_TYPA.load(Ordering::Relaxed)) << 10;
        // SAFETY: the block `init_off` declared and the `SLP_TYPa` the DSDT's `\_S5_` names, both decoded before `SOFT_OFF` was set.
        unsafe {
            cpu::outw(control, typed);
            cpu::outw(control, typed | SLP_EN);
        }
    }
    cpu::halt()
}
