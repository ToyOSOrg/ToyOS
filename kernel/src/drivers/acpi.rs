//! ACPI: the machine's half.
//!
//! The decode is `toyos-acpi`, pure and host-tested against QEMU's own tables
//! and a crafted corpus. What stays here is everything that touches the
//! machine: the direct-map reader the crate decodes through, the log lines a
//! machine owner reads a refusal off, the `Vec`s the crate cannot allocate, and
//! the PM1 port writes that turn a validated FADT into a soft-off.
//!
//! All input is firmware-supplied and untrusted: no panic on any input path,
//! every failure is a [`TableError`] and the caller decides what it means.

use alloc::vec::Vec;
use core::mem::size_of;
use core::ptr::{read_unaligned, read_volatile};
use core::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use crate::log;
use crate::DirectMap;
use toyos_acpi::{
    Century, MadtEntry, Phys, Reset, CMOS_RAM, MADT_ENTRIES, SDT_HEADER_LEN, SDT_REVISION,
};

pub use toyos_acpi::{IoApicEntry, SourceOverride, TableError};

pub struct MadtInfo {
    pub apic_ids: Vec<u32>,
    pub io_apics: Vec<IoApicEntry>,
    pub source_overrides: Vec<SourceOverride>,
}

/// x86-64's 52-bit physical-address ceiling; at or above this, `DirectMap`'s unchecked `+ PHYS_OFFSET` would wrap into the user half.
// CPUID's MAXPHYADDR may be smaller than 52 bits but never larger, so this is a safe bound on every real machine.
const MAX_PHYS: u64 = 1 << 52;

/// Firmware's physical addresses, read through the direct map.
#[derive(Clone, Copy)]
pub struct DirectPhys;

impl Phys for DirectPhys {
    fn readable(self, phys: u64, len: usize) -> bool {
        phys != 0 && phys.checked_add(len as u64).is_some_and(|end| end <= MAX_PHYS)
    }

    fn byte(self, phys: u64) -> u8 {
        // SAFETY: `readable` bounded `phys` below `MAX_PHYS`.
        unsafe { read_volatile(DirectMap::from_phys(phys).as_ptr::<u8>()) }
    }
}

/// A firmware table whose declared length has been checked to cover the read bytes, and whose declared bytes sum to zero.
#[derive(Clone, Copy)]
pub struct Table(toyos_acpi::Table<DirectPhys>);

impl Table {
    pub fn open(base: u64, signature: &[u8; 4], needed: usize) -> Result<Table, TableError> {
        toyos_acpi::Table::open(DirectPhys, base, signature, needed).map(Table)
    }

    /// The declared length, already bounded by [`Table::open`].
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// A copy of the `T` at `offset`, or `None` when the table is not that long.
    // A copy, not a reference: `&T` would assert all of `T` is valid memory — exactly the claim being checked for a struct whose tail may run past the declared length.
    pub fn field<T: Copy>(&self, offset: usize) -> Option<T> {
        let end = offset.checked_add(size_of::<T>())?;
        if end > self.0.len() {
            return None;
        }
        let at = self.0.base() + offset as u64;
        // SAFETY: `Table::open` bounded the whole declared length below `MAX_PHYS`, and `end <= len` puts this read inside it.
        Some(unsafe { read_unaligned(DirectMap::from_phys(at).as_ptr::<u8>().cast::<T>()) })
    }
}

/// The first table in the XSDT with this signature, validated for `needed` bytes.
pub fn find_table(rsdp_addr: u64, signature: &[u8; 4], needed: usize) -> Result<Table, TableError> {
    toyos_acpi::find_table(DirectPhys, rsdp_addr, signature, needed).map(Table)
}

const SLP_EN: u16 = 1 << 13;

static PM1A_CNT_PORT: AtomicU16 = AtomicU16::new(0);
static SLP_TYPA: AtomicU16 = AtomicU16::new(0);

static RESET_PORT: AtomicU16 = AtomicU16::new(0);
static RESET_VALUE: AtomicU8 = AtomicU8::new(0);

/// Log a refusal with the reason, and hand the caller `None`.
// Never a panic: a machine owner needs to see the reason, not have the kernel die on a firmware defect.
fn refuse<T>(what: &str, error: TableError) -> Option<T> {
    log!("ACPI: {what} unusable: {error:?}");
    None
}

/// Given the RSDP address from UEFI, parse XSDT -> MCFG -> return ECAM base address.
pub fn find_ecam_base(rsdp_addr: u64) -> Option<u64> {
    log!("ACPI: RSDP at {rsdp_addr:#x}");
    let (mcfg, base) = match toyos_acpi::ecam_base(DirectPhys, rsdp_addr) {
        Ok(found) => found,
        Err(e) => return refuse("MCFG", e),
    };
    log!("ACPI: MCFG found at {:#x}", mcfg.base());
    log!("ACPI: ECAM base address: {base:#x}");
    Some(base)
}

/// Parse FADT and DSDT to prepare for ACPI shutdown.
// A machine without them keeps booting without soft-off rather than panicking.
// The reset register is not decoded here: it is `init_reset`, which runs before
// the boot has anything a panic could report on.
pub fn init_power(rsdp_addr: u64) {
    const FADT_FOR_POWER: usize = toyos_acpi::FADT_PM1A_CNT_BLK + size_of::<u32>();
    const FADT_FOR_X_DSDT: usize = toyos_acpi::FADT_X_DSDT + size_of::<u64>();

    let fadt = match find_table(rsdp_addr, b"FACP", FADT_FOR_POWER) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no soft-off, shutdown will halt instead");
            return;
        }
    };

    let Some(pm1a) = fadt.field::<u32>(toyos_acpi::FADT_PM1A_CNT_BLK) else {
        log!("ACPI: FADT has no PM1a control block — no soft-off");
        return;
    };
    let pm1a = pm1a as u16;

    // Prefer X_DSDT over DSDT; a revision claiming 2.0 doesn't prove the field is present, so the length is checked rather than trusting the revision alone.
    let dsdt_addr = toyos_acpi::dsdt_address(&fadt.0);
    if dsdt_addr == 0 {
        log!(
            "ACPI: FADT names no DSDT (rev {:?}, needs {FADT_FOR_X_DSDT} bytes for X_DSDT) — no soft-off",
            fadt.field::<u8>(SDT_REVISION)
        );
        return;
    }

    let dsdt = match Table::open(dsdt_addr, b"DSDT", SDT_HEADER_LEN) {
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

/// FADT revision and the IA-PC boot architecture flags.
// `Err` is not "absent" and must not be treated as one by the caller.
pub fn iapc_boot_arch(rsdp_addr: u64) -> Result<(u8, u16), TableError> {
    toyos_acpi::iapc_boot_arch(DirectPhys, rsdp_addr)
}

/// Which CMOS register holds the RTC's century, as the FADT names it.
// `Ok(None)` is "no century register", distinct from `Err`, which the caller must not treat as one.
pub fn rtc_century_register(rsdp_addr: u64) -> Result<Option<u8>, TableError> {
    let named = toyos_acpi::rtc_century(DirectPhys, rsdp_addr)?;
    // The host can't vary what QEMU's FADT declares, so the actuator override forces "no century register" here.
    let named = if crate::actuator::rtc_no_century() { Century::Absent } else { named };

    match named {
        Century::Absent => {
            log!("ACPI: the FADT names no RTC century register");
            Ok(None)
        }
        Century::OutOfRange(index) => {
            log!(
                "ACPI: the FADT puts the RTC century register at CMOS {index:#04x}, outside {:#04x}..={:#04x} — ignoring it",
                CMOS_RAM.start(),
                CMOS_RAM.end()
            );
            Ok(None)
        }
        Century::At(index) => {
            log!("ACPI: the FADT puts the RTC century register at CMOS {index:#04x}");
            Ok(Some(index))
        }
    }
}

/// Record the FADT's reset register, or say by name why this machine has none.
///
/// The one decode of it, and it runs before `percpu::init_bsp` rather than with
/// the rest of the power tables: from the moment that function loads the IDT,
/// every panic can be reported, and a panic that can be reported but not ended
/// is a machine that still needs a hand. Walking these tables inside the panic
/// handler instead is refused — a table walk on a machine that has already
/// failed once is how a panic becomes a triple fault.
pub fn init_reset(rsdp_addr: u64) {
    let fadt = match find_table(rsdp_addr, b"FACP", toyos_acpi::FADT_FOR_RESET) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no reboot, a panic will hold the panel");
            return;
        }
    };
    match toyos_acpi::reset_register(&fadt.0) {
        Reset::Port { port, value } => {
            RESET_PORT.store(port, Ordering::Relaxed);
            RESET_VALUE.store(value, Ordering::Relaxed);
            log!("ACPI: reset register SystemIO {port:#x} <- {value:#04x}");
        }
        other => log!("ACPI: no reset register this kernel writes ({other:?}) — no reboot"),
    }
}

pub fn can_reboot() -> bool {
    RESET_PORT.load(Ordering::Relaxed) != 0
}

/// Return the machine to firmware through the FADT's reset register.
// No fallback: 0xCF9, the keyboard controller and anything else are written only where a table named them.
pub fn reboot() -> ! {
    crate::drivers::serial::flush_final();

    let port = RESET_PORT.load(Ordering::Relaxed);
    // Kernel-internal, so a bug rather than a machine quiesced and then left halted quietly.
    assert!(port != 0, "reboot: no reset register, and the caller did not ask can_reboot() first");

    // SAFETY: the port is non-zero only where `init_reset` decoded an 8-bit System I/O register, and the value is that register's.
    unsafe { crate::arch::cpu::outb(port, RESET_VALUE.load(Ordering::Relaxed)) };

    crate::arch::cpu::halt();
}

/// Trigger ACPI S5 (soft-off) shutdown.
pub fn shutdown() -> ! {
    // Last chance: nothing drains the log ring after this point.
    crate::drivers::serial::flush_final();

    let pm1a = PM1A_CNT_PORT.load(Ordering::Relaxed);
    let slp_typ = SLP_TYPA.load(Ordering::Relaxed);

    if pm1a != 0 {
        let val = (slp_typ << 10) | SLP_EN;
        // SAFETY: pm1a and slp_typ come only from the validated FADT parse via PM1A_CNT_PORT/SLP_TYPA, and the zero check above confirms that parse happened.
        unsafe { crate::arch::cpu::outw(pm1a, val) };
    }

    crate::arch::cpu::halt();
}

/// Given the RSDP address, parse XSDT -> HPET table -> return HPET MMIO base address.
pub fn find_hpet_base(rsdp_addr: u64) -> Option<u64> {
    let base = match toyos_acpi::hpet_base(DirectPhys, rsdp_addr) {
        Ok(base) => base,
        Err(e) => return refuse("HPET", e),
    };
    log!("ACPI: HPET at {base:#x}");
    Some(base)
}

/// Parse MADT (signature "APIC") to discover per-CPU APIC IDs.
pub fn parse_madt(rsdp_addr: u64) -> Option<MadtInfo> {
    let madt = match toyos_acpi::find_table(DirectPhys, rsdp_addr, b"APIC", MADT_ENTRIES) {
        Ok(table) => table,
        Err(e) => return refuse("MADT", e),
    };

    let mut apic_ids = Vec::new();
    let mut io_apics = Vec::new();
    let mut source_overrides = Vec::new();

    for item in toyos_acpi::madt_entries(&madt) {
        match item {
            Ok(MadtEntry::LocalApic { apic_id, enabled }) => {
                if enabled {
                    apic_ids.push(apic_id);
                }
            }
            Ok(MadtEntry::IoApic(entry)) => io_apics.push(entry),
            Ok(MadtEntry::SourceOverride(entry)) => source_overrides.push(entry),
            Ok(MadtEntry::Other(_)) => {}
            Err(halt) => {
                log!(
                    "ACPI: MADT entry at +{} declares {} bytes of a {}-byte list — stopping",
                    halt.at,
                    halt.declared,
                    halt.list_len
                );
                break;
            }
        }
    }

    log!("ACPI: MADT cpus={:?}", apic_ids);
    Some(MadtInfo { apic_ids, io_apics, source_overrides })
}

/// Scan DSDT AML bytecode for the \_S5_ package and extract SLP_TYPa.
// Bounded by the table's declared length via [`Table::field`].
fn find_s5_slp_typ(dsdt: &Table) -> Option<u16> {
    let s5 = b"_S5_";
    let len = dsdt.len();

    for i in SDT_HEADER_LEN..len.saturating_sub(7) {
        if (0..4).any(|j| dsdt.field::<u8>(i + j) != Some(s5[j])) {
            continue;
        }
        if dsdt.field::<u8>(i + 4) != Some(0x12) {
            continue;
        }

        let pkg_lead = dsdt.field::<u8>(i + 5)?;
        let pkg_len_bytes = match (pkg_lead >> 6) & 0x03 {
            0 => 1usize,
            n => (n + 1) as usize,
        };

        // Skip: "_S5_"(4) + PackageOp(1) + PkgLength + NumElements(1)
        let val_off = i + 4 + 1 + pkg_len_bytes + 1;
        let byte = dsdt.field::<u8>(val_off)?;
        return Some(if byte == 0x0A {
            dsdt.field::<u8>(val_off + 1)? as u16
        } else {
            byte as u16
        });
    }

    None
}
