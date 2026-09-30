//! Reset and power-off through the FADT: its reset register, and S5 soft-off
//! through the PM1a control block with the `SLP_TYPa` the DSDT's `\_S5_`
//! package names.
//!
//! All input is firmware-supplied and untrusted: a table that does not decode
//! is a machine with no reboot or no soft-off, said by name, never a panic.

use core::mem::size_of;
use core::sync::atomic::{AtomicU16, AtomicU8, Ordering};

use toyos_acpi::{Reset, Table, TableError, SDT_HEADER_LEN, SDT_REVISION};

use super::cpu;
use crate::drivers::acpi::direct_phys;
use crate::log;

const SLP_EN: u16 = 1 << 13;

static PM1A_CNT_PORT: AtomicU16 = AtomicU16::new(0);
static SLP_TYPA: AtomicU16 = AtomicU16::new(0);

static RESET_PORT: AtomicU16 = AtomicU16::new(0);
static RESET_VALUE: AtomicU8 = AtomicU8::new(0);

/// Record the FADT's reset register and its S5 soft-off, or say by name why
/// this machine has either not.
///
/// Before `percpu::init_bsp` loads the IDT: from then on every panic can be
/// reported, and a panic that can be reported but not ended is a machine that
/// still needs a hand. Walking these tables inside the panic handler instead is
/// refused — a table walk on a machine that has already failed once is how a
/// panic becomes a triple fault.
pub fn init(rsdp_addr: u64) {
    init_reset(rsdp_addr);
    init_off(rsdp_addr);
}

fn init_reset(rsdp_addr: u64) {
    let fadt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"FACP", toyos_acpi::FADT_FOR_RESET) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no reboot, a panic will hold the panel");
            return;
        }
    };
    match toyos_acpi::reset_register(&fadt) {
        Reset::Port { port, value } => {
            RESET_PORT.store(port, Ordering::Relaxed);
            RESET_VALUE.store(value, Ordering::Relaxed);
            log!("ACPI: reset register SystemIO {port:#x} <- {value:#04x}");
        }
        other => log!("ACPI: no reset register this kernel writes ({other:?}) — no reboot"),
    }
}

// A machine without these tables keeps booting without soft-off rather than panicking.
fn init_off(rsdp_addr: u64) {
    const FADT_FOR_POWER: usize = toyos_acpi::FADT_PM1A_CNT_BLK + size_of::<u32>();
    const FADT_FOR_X_DSDT: usize = toyos_acpi::FADT_X_DSDT + size_of::<u64>();

    let fadt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"FACP", FADT_FOR_POWER) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no soft-off, shutdown will halt instead");
            return;
        }
    };

    let Some(pm1a) = fadt.u32_at(toyos_acpi::FADT_PM1A_CNT_BLK) else {
        log!("ACPI: FADT has no PM1a control block — no soft-off");
        return;
    };
    let pm1a = pm1a as u16;

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

    let Some(slp_typ) = find_s5_slp_typ(&dsdt) else {
        log!("ACPI: no \\_S5_ package in the DSDT — no soft-off");
        return;
    };

    PM1A_CNT_PORT.store(pm1a, Ordering::Relaxed);
    SLP_TYPA.store(slp_typ, Ordering::Relaxed);
    log!("ACPI: PM1a={pm1a:#x} SLP_TYPa={slp_typ}");
}

pub fn can_reset() -> bool {
    RESET_PORT.load(Ordering::Relaxed) != 0
}

/// Write the reset register and nothing else: no lock, nothing but the port
/// the FADT named. A machine with no reset register halts.
// No fallback: 0xCF9, the keyboard controller and anything else are written only where a table named them.
pub fn reset() -> ! {
    let port = RESET_PORT.load(Ordering::Relaxed);
    if port != 0 {
        // SAFETY: the port is non-zero only where `init_reset` decoded an 8-bit System I/O register, and the value is that register's.
        unsafe { cpu::outb(port, RESET_VALUE.load(Ordering::Relaxed)) };
    }
    cpu::halt()
}

/// Enter S5, or halt on a machine whose tables named no soft-off.
pub fn off() -> ! {
    let pm1a = PM1A_CNT_PORT.load(Ordering::Relaxed);
    if pm1a != 0 {
        let val = (SLP_TYPA.load(Ordering::Relaxed) << 10) | SLP_EN;
        // SAFETY: pm1a and slp_typ come only from the validated FADT parse via PM1A_CNT_PORT/SLP_TYPA, and the zero check above confirms that parse happened.
        unsafe { cpu::outw(pm1a, val) };
    }
    cpu::halt()
}

/// Scan DSDT AML bytecode for the \_S5_ package and extract SLP_TYPa.
// Bounded by the table's declared length via [`Table::byte`].
fn find_s5_slp_typ<P: toyos_acpi::Phys>(dsdt: &Table<P>) -> Option<u16> {
    let s5 = b"_S5_";
    let len = dsdt.len();

    for i in SDT_HEADER_LEN..len.saturating_sub(7) {
        if (0..4).any(|j| dsdt.byte(i + j) != Some(s5[j])) {
            continue;
        }
        if dsdt.byte(i + 4) != Some(0x12) {
            continue;
        }

        let pkg_lead = dsdt.byte(i + 5)?;
        let pkg_len_bytes = match (pkg_lead >> 6) & 0x03 {
            0 => 1usize,
            n => (n + 1) as usize,
        };

        // Skip: "_S5_"(4) + PackageOp(1) + PkgLength + NumElements(1)
        let val_off = i + 4 + 1 + pkg_len_bytes + 1;
        let byte = dsdt.byte(val_off)?;
        return Some(if byte == 0x0A {
            dsdt.byte(val_off + 1)? as u16
        } else {
            byte as u16
        });
    }

    None
}
