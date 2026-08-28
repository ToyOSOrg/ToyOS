//! ACPI table parsing.
//!
//! All input is firmware-supplied and untrusted: no panic on any input path,
//! every failure is a [`TableError`] and the caller decides what it means.
//! Nothing reads a table except through [`Table::open`], which is what makes
//! the bounds hold by construction.

use alloc::vec::Vec;
use core::mem::{offset_of, size_of};
use core::ptr::{read_unaligned, read_volatile};
use core::sync::atomic::{AtomicU16, Ordering};
use crate::log;
use crate::DirectMap;

pub struct MadtInfo {
    pub apic_ids: Vec<u32>,
    pub io_apics: Vec<IoApicEntry>,
    pub source_overrides: Vec<SourceOverride>,
}

/// MADT type 1: an I/O APIC's register window and its first GSI.
pub struct IoApicEntry {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

/// MADT type 2: an ISA IRQ override.
pub struct SourceOverride {
    pub bus: u8,
    pub source_irq: u8,
    pub gsi: u32,
    /// The raw MPS INTI word (bits 0-1 polarity, 2-3 trigger).
    pub flags: u16,
}

/// Why a firmware table cannot be used; each variant is a distinct instruction to the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableError {
    /// The RSDP UEFI handed us has the wrong signature or does not checksum.
    BadRsdp,
    /// An ACPI 1.0 root pointer, or a null XSDT address.
    // There is no RSDT fallback: this kernel is UEFI-only and every machine it targets publishes an XSDT.
    NoXsdt,
    /// No table with that signature in the XSDT.
    Absent,
    /// The declared length cannot hold the fields the caller reads, or is implausible.
    Length { declared: u32, needed: usize },
    /// The declared bytes do not sum to zero.
    // Which table failed isn't carried here: every call site already names the table in its own log line.
    Checksum,
}

/// Largest length a table may declare — bounds the checksum walk and every derived entry count.
const MAX_TABLE_LEN: usize = 1024 * 1024;

/// x86-64's 52-bit physical-address ceiling; at or above this, `DirectMap`'s unchecked `+ PHYS_OFFSET` would wrap into the user half.
// CPUID's MAXPHYADDR may be smaller than 52 bits but never larger, so this is a safe bound on every real machine.
const MAX_PHYS: u64 = 1 << 52;

/// The only constructor of [`Mapped`]: a firmware address is not dereferenceable until it passes through here.
fn table_at(phys: u64) -> Option<Mapped> {
    (phys != 0 && phys < MAX_PHYS).then(|| Mapped(DirectMap::from_phys(phys)))
}

/// A firmware-supplied address that [`table_at`] has bounded to a mapped, in-range pointer — not a claim a table lives there.
#[derive(Clone, Copy)]
pub struct Mapped(DirectMap);

impl Mapped {
    /// A copy of the `T` at `offset`, unaligned — ACPI tables are byte-packed.
    fn field<T: Copy>(self, offset: usize) -> T {
        // SAFETY: `Mapped` is only built by `table_at`, which bounds `phys` below `MAX_PHYS` where the direct map covers it, and `offset` is bounded by the caller against the table's validated length.
        unsafe { read_unaligned(self.0.as_ptr::<u8>().add(offset).cast::<T>()) }
    }

    /// One byte, volatile — read by the checksum walk.
    fn byte(self, offset: usize) -> u8 {
        // SAFETY: sound on the same bound as `field`; `offset` is `0..len` where the caller bounded `len` by `MAX_TABLE_LEN` or `RSDP_MAX_LEN`.
        unsafe { read_volatile(self.0.as_ptr::<u8>().add(offset)) }
    }
}

impl core::fmt::Display for Mapped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

// All packed, never dereferenced — used only via `offset_of!`, since a reference would assert readability the firmware hasn't proved.

#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    _reserved: [u8; 3],
}

#[repr(C, packed)]
struct Fadt {
    header: SdtHeader,
    firmware_ctrl: u32,
    dsdt: u32,
    _reserved0: u8,
    preferred_pm_profile: u8,
    sci_interrupt: u16,
    smi_command_port: u32,
    acpi_enable: u8,
    acpi_disable: u8,
    s4bios_req: u8,
    pstate_control: u8,
    pm1a_event_block: u32,
    pm1b_event_block: u32,
    pm1a_control_block: u32,
    pm1b_control_block: u32,
    pm2_control_block: u32,
    pm_timer_block: u32,
    gpe0_block: u32,
    gpe1_block: u32,
    pm1_event_length: u8,
    pm1_control_length: u8,
    pm2_control_length: u8,
    pm_timer_length: u8,
    gpe0_block_length: u8,
    gpe1_block_length: u8,
    gpe1_base: u8,
    c_state_control: u8,
    worst_c2_latency: u16,
    worst_c3_latency: u16,
    flush_size: u16,
    flush_stride: u16,
    duty_offset: u8,
    duty_width: u8,
    day_alarm: u8,
    month_alarm: u8,
    century: u8,
    iapc_boot_arch: u16,
    _reserved1: u8,
    flags: u32,
    reset_reg: [u8; 12], // Generic Address Structure
    reset_value: u8,
    arm_boot_arch: u16,
    fadt_minor_version: u8,
    x_firmware_ctrl: u64,
    x_dsdt: u64,
}

#[repr(C, packed)]
struct Madt {
    header: SdtHeader,
    local_apic_address: u32,
    flags: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtEntryHeader {
    entry_type: u8,
    length: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtLocalApic {
    header: MadtEntryHeader,
    processor_id: u8,
    apic_id: u8,
    flags: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtIoApic {
    header: MadtEntryHeader,
    io_apic_id: u8,
    _reserved: u8,
    address: u32,
    gsi_base: u32,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtSourceOverride {
    header: MadtEntryHeader,
    bus: u8,
    source_irq: u8,
    gsi: u32,
    flags: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct MadtLocalX2Apic {
    header: MadtEntryHeader,
    _reserved: u16,
    x2apic_id: u32,
    flags: u32,
    _processor_uid: u32,
}

#[repr(C, packed)]
struct McfgEntry {
    base_address: u64,
    segment_group: u16,
    start_bus: u8,
    end_bus: u8,
    _reserved: u32,
}

#[repr(C, packed)]
struct HpetTable {
    header: SdtHeader,
    event_timer_block_id: u32,
    base_address: [u8; 4], // Generic Address Structure prefix (address_space, bit_width, bit_offset, access_size)
    base_address_value: u64,
}

/// The MCFG's first allocation structure sits one 8-byte reserved field past the header.
const MCFG_FIRST_ENTRY: usize = size_of::<SdtHeader>() + 8;

const SLP_EN: u16 = 1 << 13;

static PM1A_CNT_PORT: AtomicU16 = AtomicU16::new(0);
static SLP_TYPA: AtomicU16 = AtomicU16::new(0);

/// A firmware table whose declared length has been checked to cover the read bytes, and whose declared bytes sum to zero.
#[derive(Clone, Copy)]
pub struct Table {
    base: Mapped,
    len: usize,
}

impl Table {
    /// Order matters: the checksum walk depends on the length already being bounded.
    fn open(base: Mapped, signature: &[u8; 4], needed: usize) -> Result<Table, TableError> {
        let found: [u8; 4] = base.field(offset_of!(SdtHeader, signature));
        if &found != signature {
            return Err(TableError::Absent);
        }
        let declared: u32 = base.field(offset_of!(SdtHeader, length));
        let len = declared as usize;
        let floor = needed.max(size_of::<SdtHeader>());
        if len < floor || len > MAX_TABLE_LEN {
            return Err(TableError::Length { declared, needed: floor });
        }
        if !sums_to_zero(base, len) {
            return Err(TableError::Checksum);
        }
        Ok(Table { base, len })
    }

    /// The declared length, already bounded by [`Table::open`].
    pub fn len(&self) -> usize {
        self.len
    }

    /// A copy of the `T` at `offset`, or `None` when the table is not that long.
    // A copy, not a reference: `&T` would assert all of `T` is valid memory — exactly the claim being checked for a struct whose tail may run past the declared length.
    pub fn field<T: Copy>(&self, offset: usize) -> Option<T> {
        let end = offset.checked_add(size_of::<T>())?;
        if end > self.len {
            return None;
        }
        Some(self.base.field(offset))
    }

    /// Like `field`, for a caller that has already proved `offset` is in bounds.
    fn field_unchecked<T: Copy>(&self, offset: usize) -> T {
        self.base.field(offset)
    }
}

/// Intact when the declared bytes sum to zero in 8 bits.
// `len` is bounded by the caller.
fn sums_to_zero(base: Mapped, len: usize) -> bool {
    let mut sum: u8 = 0;
    for i in 0..len {
        sum = sum.wrapping_add(base.byte(i));
    }
    sum == 0
}

/// The XSDT, validated, from the RSDP UEFI handed the bootloader.
fn xsdt(rsdp_addr: u64) -> Result<Table, TableError> {
    const RSDP_V1_LEN: usize = 20;
    /// An RSDP is 36 bytes; the bound stops the extended checksum from being pointed at the whole map.
    const RSDP_MAX_LEN: usize = 64;

    let base = table_at(rsdp_addr).ok_or(TableError::BadRsdp)?;
    let signature: [u8; 8] = base.field(0);
    if &signature != b"RSD PTR " {
        return Err(TableError::BadRsdp);
    }
    // The ACPI 1.0 part is a fixed 20 bytes; only after it checksums is the 2.0 extension read.
    if !sums_to_zero(base, RSDP_V1_LEN) {
        return Err(TableError::BadRsdp);
    }

    let revision: u8 = base.field(offset_of!(Rsdp, revision));
    if revision < 2 {
        return Err(TableError::NoXsdt);
    }

    let declared: u32 = base.field(offset_of!(Rsdp, length));
    let len = declared as usize;
    if len < size_of::<Rsdp>() || len > RSDP_MAX_LEN {
        return Err(TableError::Length { declared, needed: size_of::<Rsdp>() });
    }
    if !sums_to_zero(base, len) {
        return Err(TableError::BadRsdp);
    }

    let address: u64 = base.field(offset_of!(Rsdp, xsdt_address));
    let root = table_at(address).ok_or(TableError::NoXsdt)?;
    Table::open(root, b"XSDT", size_of::<SdtHeader>())
}

/// The first table in the XSDT with this signature, validated for `needed` bytes.
// A second match with the same signature never replaces an invalid first.
pub fn find_table(rsdp_addr: u64, signature: &[u8; 4], needed: usize) -> Result<Table, TableError> {
    let xsdt = xsdt(rsdp_addr)?;
    // Table::open guarantees len >= size_of::<SdtHeader>(), so this subtraction is total.
    let entry_count = (xsdt.len - size_of::<SdtHeader>()) / size_of::<u64>();

    for i in 0..entry_count {
        let offset = size_of::<SdtHeader>() + i * size_of::<u64>();
        let Some(phys) = xsdt.field::<u64>(offset) else { break };
        let Some(base) = table_at(phys) else { continue };
        match Table::open(base, signature, needed) {
            Err(TableError::Absent) => continue,
            other => return other,
        }
    }
    Err(TableError::Absent)
}

/// Log a refusal with the reason, and hand the caller `None`.
// Never a panic: a machine owner needs to see the reason, not have the kernel die on a firmware defect.
fn refuse<T>(what: &str, error: TableError) -> Option<T> {
    log!("ACPI: {what} unusable: {error:?}");
    None
}

/// Given the RSDP address from UEFI, parse XSDT -> MCFG -> return ECAM base address.
pub fn find_ecam_base(rsdp_addr: u64) -> Option<u64> {
    log!("ACPI: RSDP at {rsdp_addr:#x}");
    let needed = MCFG_FIRST_ENTRY + size_of::<McfgEntry>();
    let mcfg = match find_table(rsdp_addr, b"MCFG", needed) {
        Ok(table) => table,
        Err(e) => return refuse("MCFG", e),
    };
    log!("ACPI: MCFG found at {}", mcfg.base);

    let ecam_base = mcfg.field::<u64>(MCFG_FIRST_ENTRY + offset_of!(McfgEntry, base_address))?;
    log!("ACPI: ECAM base address: {ecam_base:#x}");
    Some(ecam_base)
}

/// Parse FADT and DSDT to prepare for ACPI shutdown.
// A machine without them keeps booting without soft-off rather than panicking.
pub fn init_power(rsdp_addr: u64) {
    const FADT_FOR_POWER: usize =
        offset_of!(Fadt, pm1a_control_block) + size_of::<u32>();
    const FADT_FOR_X_DSDT: usize = offset_of!(Fadt, x_dsdt) + size_of::<u64>();

    let fadt = match find_table(rsdp_addr, b"FACP", FADT_FOR_POWER) {
        Ok(table) => table,
        Err(e) => {
            log!("ACPI: FADT unusable: {e:?} — no soft-off, shutdown will halt instead");
            return;
        }
    };

    let Some(pm1a) = fadt.field::<u32>(offset_of!(Fadt, pm1a_control_block)) else {
        log!("ACPI: FADT has no PM1a control block — no soft-off");
        return;
    };
    let pm1a = pm1a as u16;

    // Prefer X_DSDT over DSDT; a revision claiming 2.0 doesn't prove the field is present, so the length is checked rather than trusting the revision alone.
    let revision = fadt.field::<u8>(offset_of!(Fadt, header) + offset_of!(SdtHeader, revision));
    let x_dsdt = match revision {
        Some(r) if r >= 2 => fadt.field::<u64>(offset_of!(Fadt, x_dsdt)).filter(|a| *a != 0),
        _ => None,
    };
    let dsdt_addr = match x_dsdt {
        Some(addr) => addr,
        None => fadt.field::<u32>(offset_of!(Fadt, dsdt)).unwrap_or(0) as u64,
    };
    if dsdt_addr == 0 {
        log!(
            "ACPI: FADT names no DSDT (rev {:?}, needs {FADT_FOR_X_DSDT} bytes for X_DSDT) — no soft-off",
            revision
        );
        return;
    }

    let Some(dsdt_base) = table_at(dsdt_addr) else {
        log!("ACPI: FADT points the DSDT at {dsdt_addr:#x}, which is not an address — no soft-off");
        return;
    };
    let dsdt = match Table::open(dsdt_base, b"DSDT", size_of::<SdtHeader>()) {
        Ok(table) => table,
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
// Bit 1 of the flags is the port 60/64 keyboard-controller bit, defined only from FADT revision 3 onward.
pub fn iapc_boot_arch(rsdp_addr: u64) -> Result<(u8, u16), TableError> {
    const NEEDED: usize = offset_of!(Fadt, iapc_boot_arch) + size_of::<u16>();
    let fadt = find_table(rsdp_addr, b"FACP", NEEDED)?;
    let revision = fadt
        .field::<u8>(offset_of!(Fadt, header) + offset_of!(SdtHeader, revision))
        .ok_or(TableError::Length { declared: fadt.len as u32, needed: NEEDED })?;
    let flags = fadt
        .field::<u16>(offset_of!(Fadt, iapc_boot_arch))
        .ok_or(TableError::Length { declared: fadt.len as u32, needed: NEEDED })?;
    Ok((revision, flags))
}

/// Which CMOS register holds the RTC's century, as the FADT names it.
// `Ok(None)` is "no century register", distinct from `Err`, which the caller must not treat as one.
pub fn rtc_century_register(rsdp_addr: u64) -> Result<Option<u8>, TableError> {
    // 0x80+ selects with NMI-mask bit 7 set; below 0x0E is the RTC's own clock/status regs, not a century register.
    const CMOS_RAM: core::ops::RangeInclusive<u8> = 0x0E..=0x7F;

    const NEEDED: usize = offset_of!(Fadt, century) + size_of::<u8>();
    let fadt = find_table(rsdp_addr, b"FACP", NEEDED)?;
    let declared = fadt
        .field::<u8>(offset_of!(Fadt, century))
        .ok_or(TableError::Length { declared: fadt.len as u32, needed: NEEDED })?;
    // The host can't vary what QEMU's FADT declares, so the actuator override forces "no century register" here.
    let index = if crate::actuator::rtc_no_century() { 0 } else { declared };

    if index == 0 {
        log!("ACPI: the FADT names no RTC century register");
        return Ok(None);
    }
    if !CMOS_RAM.contains(&index) {
        log!(
            "ACPI: the FADT puts the RTC century register at CMOS {index:#04x}, outside {:#04x}..={:#04x} — ignoring it",
            CMOS_RAM.start(),
            CMOS_RAM.end()
        );
        return Ok(None);
    }
    log!("ACPI: the FADT puts the RTC century register at CMOS {index:#04x}");
    Ok(Some(index))
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
    let needed = offset_of!(HpetTable, base_address_value) + size_of::<u64>();
    let hpet = match find_table(rsdp_addr, b"HPET", needed) {
        Ok(table) => table,
        Err(e) => return refuse("HPET", e),
    };
    let base = hpet.field::<u64>(offset_of!(HpetTable, base_address_value))?;
    log!("ACPI: HPET at {base:#x}");
    Some(base)
}

/// Parse MADT (signature "APIC") to discover per-CPU APIC IDs.
pub fn parse_madt(rsdp_addr: u64) -> Option<MadtInfo> {
    let madt = match find_table(rsdp_addr, b"APIC", size_of::<Madt>()) {
        Ok(table) => table,
        Err(e) => return refuse("MADT", e),
    };

    let mut apic_ids = Vec::new();
    let mut io_apics = Vec::new();
    let mut source_overrides = Vec::new();

    // Total because Table::open was given size_of::<Madt>() as the floor.
    let entries_len = madt.len - size_of::<Madt>();
    let mut offset = 0usize;

    while offset + size_of::<MadtEntryHeader>() <= entries_len {
        let base = size_of::<Madt>() + offset;
        let entry: MadtEntryHeader = madt.field_unchecked(base);
        let entry_len = entry.length as usize;
        // A zero-length or overrunning entry can't be resynchronised, so both end the walk.
        if entry_len < size_of::<MadtEntryHeader>() || offset + entry_len > entries_len {
            log!(
                "ACPI: MADT entry at +{offset} declares {entry_len} bytes of a {entries_len}-byte list — stopping"
            );
            break;
        }

        match entry.entry_type {
            0 if entry_len >= size_of::<MadtLocalApic>() => {
                let lapic: MadtLocalApic = madt.field_unchecked(base);
                if lapic.flags & 1 != 0 {
                    apic_ids.push(lapic.apic_id as u32);
                }
            }
            1 if entry_len >= size_of::<MadtIoApic>() => {
                let io: MadtIoApic = madt.field_unchecked(base);
                io_apics.push(IoApicEntry {
                    id: io.io_apic_id,
                    address: io.address,
                    gsi_base: io.gsi_base,
                });
            }
            2 if entry_len >= size_of::<MadtSourceOverride>() => {
                let iso: MadtSourceOverride = madt.field_unchecked(base);
                source_overrides.push(SourceOverride {
                    bus: iso.bus,
                    source_irq: iso.source_irq,
                    gsi: iso.gsi,
                    flags: iso.flags,
                });
            }
            9 if entry_len >= size_of::<MadtLocalX2Apic>() => {
                let x2: MadtLocalX2Apic = madt.field_unchecked(base);
                if x2.flags & 1 != 0 {
                    apic_ids.push(x2.x2apic_id);
                }
            }
            _ => {}
        }

        offset += entry_len;
    }

    log!("ACPI: MADT cpus={:?}", apic_ids);
    Some(MadtInfo { apic_ids, io_apics, source_overrides })
}

/// Scan DSDT AML bytecode for the \_S5_ package and extract SLP_TYPa.
// Bounded by the table's declared length via [`Table::field`].
fn find_s5_slp_typ(dsdt: &Table) -> Option<u16> {
    let s5 = b"_S5_";
    let len = dsdt.len;

    for i in size_of::<SdtHeader>()..len.saturating_sub(7) {
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
