//! The FADT (signature `FACP`): ACPI 6.5 §5.2.9, Table 5.9.

use core::num::NonZeroU8;

use crate::{find_table, Phys, Table, TableError, SDT_REVISION};

/// Table 5.9 offsets, from the start of the table.
pub(crate) const FADT_FIRMWARE_CTRL: usize = 36;
pub const FADT_DSDT: usize = 40;
pub const FADT_PM1A_CNT_BLK: usize = 64;
const FADT_CENTURY: usize = 108;
const FADT_IAPC_BOOT_ARCH: usize = 109;
const FADT_FLAGS: usize = 112;
/// A Generic Address Structure (§5.2.3.2): space at +0, bit width at +1, bit offset at +2, address at +4.
const FADT_RESET_REG: usize = 116;
const FADT_RESET_VALUE: usize = 128;
const FADT_ARM_BOOT_ARCH: usize = 129;
const FADT_MINOR_VERSION: usize = 131;
pub(crate) const FADT_X_FIRMWARE_CTRL: usize = 132;
const FADT_X_DSDT: usize = 140;

/// `ARM_BOOT_ARCH`'s two flags.
const PSCI_COMPLIANT: u16 = 1 << 0;
const PSCI_USE_HVC: u16 = 1 << 1;

/// The bytes [`reset_register`] reads to the end of, so a caller opening the
/// table for that decode alone asks for what it needs and nothing after it.
pub const FADT_FOR_RESET: usize = FADT_RESET_VALUE + size_of::<u8>();

/// Fixed feature flags bit 10, `RESET_REG_SUP` (Table 5.10); then the address space IDs of Table 5.1.
const RESET_REG_SUP: u32 = 1 << 10;
const SPACE_SYSTEM_MEMORY: u8 = 0;
const SPACE_SYSTEM_IO: u8 = 1;
const SPACE_PCI_CONFIG: u8 = 2;

// `Err` is not "absent" and must not be treated as one by the caller.
// Bit 1 of the flags is the port 60/64 keyboard-controller bit, defined only from FADT revision 3 onward.
pub fn iapc_boot_arch<P: Phys>(phys: P, rsdp_addr: u64) -> Result<(u8, u16), TableError> {
    const NEEDED: usize = FADT_IAPC_BOOT_ARCH + 2;
    let fadt = find_table(phys, rsdp_addr, b"FACP", NEEDED)?;
    let short = || TableError::Length { declared: fadt.len() as u32, needed: NEEDED };
    let revision = fadt.byte(SDT_REVISION).ok_or_else(short)?;
    let flags = fadt.u16_at(FADT_IAPC_BOOT_ARCH).ok_or_else(short)?;
    Ok((revision, flags))
}

/// What the FADT says about the RTC's century register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Century {
    /// The century field is zero: this firmware names no register.
    Absent,
    /// Outside CMOS RAM, so it is not a century register whatever it is.
    OutOfRange(u8),
    At(u8),
}

/// 0x80+ selects with NMI-mask bit 7 set; below 0x0E is the RTC's own clock/status regs, not a century register.
pub const CMOS_RAM: core::ops::RangeInclusive<u8> = 0x0E..=0x7F;

/// Which CMOS register holds the RTC's century, as the FADT names it.
pub fn rtc_century<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Century, TableError> {
    const NEEDED: usize = FADT_CENTURY + 1;
    let fadt = find_table(phys, rsdp_addr, b"FACP", NEEDED)?;
    let declared = fadt
        .byte(FADT_CENTURY)
        .ok_or(TableError::Length { declared: fadt.len() as u32, needed: NEEDED })?;
    Ok(century_of(declared))
}

pub fn century_of(index: u8) -> Century {
    match index {
        0 => Century::Absent,
        i if CMOS_RAM.contains(&i) => Century::At(i),
        i => Century::OutOfRange(i),
    }
}

/// The 8-bit System I/O port the FADT names and the byte to write there, or the
/// field that refused one — the alternative being a guessed port, and a guess
/// writes a byte to whatever else lives there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reset {
    Port { port: u16, value: u8 },
    Absent,
    Unsupported,
    SystemMemory,
    PciConfig,
    Space(u8),
    Field { bit_width: u8, bit_offset: u8 },
    /// Zero, or past the 16-bit port space: not a port this kernel writes.
    Address(u64),
}

pub fn reset_register<P: Phys>(fadt: &Table<P>) -> Reset {
    // Revision 3 is where Table 5.9 puts these fields, and a table stopping short of them has none either.
    if !matches!(fadt.byte(SDT_REVISION), Some(r) if r >= 3) {
        return Reset::Absent;
    }
    let fields = || {
        Some((
            fadt.u32_at(FADT_FLAGS)?,
            fadt.byte(FADT_RESET_REG)?,
            fadt.byte(FADT_RESET_REG + 1)?,
            fadt.byte(FADT_RESET_REG + 2)?,
            fadt.u64_at(FADT_RESET_REG + 4)?,
            fadt.byte(FADT_RESET_VALUE)?,
        ))
    };
    let Some((flags, space, bit_width, bit_offset, address, value)) = fields() else {
        return Reset::Absent;
    };
    if flags & RESET_REG_SUP == 0 {
        return Reset::Unsupported;
    }
    match space {
        SPACE_SYSTEM_MEMORY => return Reset::SystemMemory,
        SPACE_PCI_CONFIG => return Reset::PciConfig,
        SPACE_SYSTEM_IO => {}
        other => return Reset::Space(other),
    }
    // The bit width, not the GAS access size: firmware may leave that 0 (undefined).
    if (bit_width, bit_offset) != (8, 0) {
        return Reset::Field { bit_width, bit_offset };
    }
    match u16::try_from(address) {
        Ok(port) if port != 0 => Reset::Port { port, value },
        _ => Reset::Address(address),
    }
}

/// How the FADT says the Power State Coordination Interface is reached.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Psci {
    /// `PSCI_COMPLIANT`, through `SMC`.
    Smc,
    /// `PSCI_COMPLIANT` and `PSCI_USE_HVC`.
    Hvc,
    /// `PSCI_COMPLIANT` clear: firmware offers no PSCI.
    Absent,
    /// A FADT before ACPI 5.1: those bytes are reserved, so nothing is said
    /// about PSCI at all.
    Undefined { revision: u8, minor: u8 },
    /// A FADT that ends before `ARM_BOOT_ARCH` and the minor version after it.
    Short,
}

/// `ARM_BOOT_ARCH`, read only from a FADT whose version defines it: 5.1 on,
/// the version its header's major and `FADT Minor Version`'s low nibble spell.
pub fn psci<P: Phys>(fadt: &Table<P>) -> Psci {
    let revision = fadt.byte(SDT_REVISION).unwrap_or(0);
    let (Some(flags), Some(minor)) = (fadt.u16_at(FADT_ARM_BOOT_ARCH), fadt.byte(FADT_MINOR_VERSION)) else {
        return Psci::Short;
    };
    let minor = minor & 0xF;
    if revision < 5 || (revision == 5 && minor < 1) {
        return Psci::Undefined { revision, minor };
    }
    match flags {
        flags if flags & PSCI_COMPLIANT == 0 => Psci::Absent,
        flags if flags & PSCI_USE_HVC != 0 => Psci::Hvc,
        _ => Psci::Smc,
    }
}

pub fn dsdt_address<P: Phys>(fadt: &Table<P>) -> u64 {
    let x_dsdt = match fadt.byte(SDT_REVISION) {
        Some(r) if r >= 2 => fadt.u64_at(FADT_X_DSDT).filter(|a| *a != 0),
        _ => None,
    };
    match x_dsdt {
        Some(addr) => addr,
        None => u64::from(fadt.u32_at(FADT_DSDT).unwrap_or(0)),
    }
}

/// Table 5.9 offsets of the fixed-hardware fields.
const FADT_SCI_INT: usize = 46;
const FADT_SMI_CMD: usize = 48;
const FADT_ACPI_ENABLE: usize = 52;
const FADT_ACPI_DISABLE: usize = 53;
const FADT_PM1A_EVT_BLK: usize = 56;
const FADT_PM1B_EVT_BLK: usize = 60;
const FADT_PM1B_CNT_BLK: usize = 68;
const FADT_GPE0_BLK: usize = 80;
const FADT_GPE1_BLK: usize = 84;
const FADT_PM1_EVT_LEN: usize = 88;
const FADT_PM1_CNT_LEN: usize = 89;
const FADT_GPE0_BLK_LEN: usize = 92;
const FADT_GPE1_BLK_LEN: usize = 93;
const FADT_X_PM1A_EVT_BLK: usize = 148;
const FADT_X_PM1B_EVT_BLK: usize = 160;
const FADT_X_PM1A_CNT_BLK: usize = 172;
const FADT_X_PM1B_CNT_BLK: usize = 184;
const FADT_X_GPE0_BLK: usize = 220;
const FADT_X_GPE1_BLK: usize = 232;
/// The end of `X_GPE1_BLK`, the last field [`fixed_hardware`] reads where the
/// table holds it.
const FADT_X_END: usize = FADT_X_GPE1_BLK + 12;

/// Table 5.10's `PWR_BUTTON` (bit 4): set, the power button is a control
/// method device, which only AML serves; and `HW_REDUCED_ACPI` (bit 20).
const PWR_BUTTON: u32 = 1 << 4;
const HW_REDUCED_ACPI: u32 = 1 << 20;

/// The bytes [`fixed_hardware`] needs at least: through the flags.
pub const FADT_FOR_FIXED_HARDWARE: usize = FADT_FLAGS + 4;

/// What the FADT's power button is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerButton {
    /// `PWRBTN_STS` and `PWRBTN_EN` in the PM1 event block.
    Fixed,
    /// A control method device, which only AML serves.
    ControlMethod,
}

/// The way out of legacy mode and back into it: the port, and the value
/// written to it for each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LegacyMode {
    pub smi_cmd: u16,
    pub acpi_enable: NonZeroU8,
    pub acpi_disable: NonZeroU8,
}

/// The fixed hardware an OS serves the SCI through, as the FADT names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FixedHardware {
    pub sci_int: u16,
    /// `None` where the FADT leaves `SMI_CMD`, `ACPI_ENABLE` or `ACPI_DISABLE`
    /// zero: Table 5.9 reserves each as zero on a machine without legacy mode,
    /// and one that names a way in and no way back is not taken in.
    pub legacy: Option<LegacyMode>,
    pub pm1a_event: toyos_abi::acpi::Block,
    /// [`toyos_abi::acpi::Block::NONE`] where the machine has no GPE0 block.
    pub gpe0: toyos_abi::acpi::Block,
    pub power_button: PowerButton,
}

/// Which field a [`FixedRefused`] is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Pm1aEvent,
    Pm1aControl,
    Gpe0,
}

/// Why the FADT's fixed hardware is none this kernel serves, one reason each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixedRefused {
    /// The table ends before the flags.
    Short { len: usize },
    /// `HW_REDUCED_ACPI`: no fixed hardware, its events come through a GED.
    HardwareReduced,
    /// A PM1b event or control block, written in step with PM1a's: not served.
    Pm1b,
    /// A GPE1 block: not served.
    Gpe1,
    /// Neither field names the block.
    Absent { field: Field },
    /// The block is shorter than its registers.
    Length { field: Field, len: u8 },
    /// The `X_` block is not in the System I/O space.
    NotSystemIo { field: Field, space: u8 },
    /// The `X_` block and the 32-bit one both name an address, and not the same.
    Disagrees { field: Field, legacy: u32, extended: u64 },
    /// The block runs past the 16-bit port space.
    PastPorts { field: Field, address: u64 },
    /// `SMI_CMD` names a port past the 16-bit space.
    SmiCmd(u32),
}

/// Whether the table holds the `X_` fields [`fixed_hardware`] reads.
fn has_x<P: Phys>(fadt: &Table<P>) -> bool {
    fadt.len() >= FADT_X_END && matches!(fadt.byte(SDT_REVISION), Some(r) if r >= 2)
}

/// A block's address, the 32-bit field's or its `X_` twin's.
fn address<P: Phys>(fadt: &Table<P>, field: Field, legacy_at: usize, x_at: usize) -> Result<u64, FixedRefused> {
    let short = FixedRefused::Short { len: fadt.len() };
    let legacy = fadt.u32_at(legacy_at).ok_or(short)?;
    if !has_x(fadt) {
        return Ok(u64::from(legacy));
    }
    let space = fadt.byte(x_at).ok_or(short)?;
    let extended = fadt.u64_at(x_at + 4).ok_or(short)?;
    if extended == 0 {
        return Ok(u64::from(legacy));
    }
    if space != SPACE_SYSTEM_IO {
        return Err(FixedRefused::NotSystemIo { field, space });
    }
    if legacy != 0 && u64::from(legacy) != extended {
        return Err(FixedRefused::Disagrees { field, legacy, extended });
    }
    Ok(extended)
}

fn block(field: Field, address: u64, len: u8) -> Result<toyos_abi::acpi::Block, FixedRefused> {
    match u16::try_from(address) {
        Ok(port) if u32::from(port) + u32::from(len) <= 0x1_0000 => Ok(toyos_abi::acpi::Block { port, len: u16::from(len) }),
        _ => Err(FixedRefused::PastPorts { field, address }),
    }
}

/// The PM1a control block, checked against its `X_` twin where the table
/// holds one; its length the `PM1_CNT_LEN` byte's.
pub fn pm1a_control<P: Phys>(fadt: &Table<P>) -> Result<toyos_abi::acpi::Block, FixedRefused> {
    // §4.8.1: a control block is at least two bytes.
    let len = fadt.byte(FADT_PM1_CNT_LEN).ok_or(FixedRefused::Short { len: fadt.len() })?;
    let at = address(fadt, Field::Pm1aControl, FADT_PM1A_CNT_BLK, FADT_X_PM1A_CNT_BLK)?;
    if at == 0 {
        return Err(FixedRefused::Absent { field: Field::Pm1aControl });
    }
    if len < 2 {
        return Err(FixedRefused::Length { field: Field::Pm1aControl, len });
    }
    block(Field::Pm1aControl, at, len)
}

/// The FADT's fixed-hardware event blocks, each checked against its `X_`
/// twin where the table holds one; lengths are the `*_LEN` bytes', never a
/// Generic Address Structure's bit width, which firmware leaves 0. The
/// control block is [`pm1a_control`]'s.
pub fn fixed_hardware<P: Phys>(fadt: &Table<P>) -> Result<FixedHardware, FixedRefused> {
    use toyos_abi::acpi::Block;
    let short = FixedRefused::Short { len: fadt.len() };
    let flags = fadt.u32_at(FADT_FLAGS).ok_or(short)?;
    if flags & HW_REDUCED_ACPI != 0 {
        return Err(FixedRefused::HardwareReduced);
    }
    let x = has_x(fadt);
    let u32_at = |at| fadt.u32_at(at).ok_or(short);
    let byte = |at| fadt.byte(at).ok_or(short);

    // Named in either field, in whatever space.
    let present = |legacy_at, x_at: usize| -> Result<bool, FixedRefused> {
        Ok(u32_at(legacy_at)? != 0 || (x && fadt.u64_at(x_at + 4).ok_or(short)? != 0))
    };
    if present(FADT_PM1B_EVT_BLK, FADT_X_PM1B_EVT_BLK)? || present(FADT_PM1B_CNT_BLK, FADT_X_PM1B_CNT_BLK)? {
        return Err(FixedRefused::Pm1b);
    }
    if present(FADT_GPE1_BLK, FADT_X_GPE1_BLK)? || byte(FADT_GPE1_BLK_LEN)? != 0 {
        return Err(FixedRefused::Gpe1);
    }

    // §4.8.1: a PM1 event block is a status and an enable register of at least
    // two bytes each, and a GPE block a status half and an enable half.
    let pm1_event_len = byte(FADT_PM1_EVT_LEN)?;
    let pm1_event = address(fadt, Field::Pm1aEvent, FADT_PM1A_EVT_BLK, FADT_X_PM1A_EVT_BLK)?;
    if pm1_event == 0 {
        return Err(FixedRefused::Absent { field: Field::Pm1aEvent });
    }
    if pm1_event_len < 4 || pm1_event_len % 2 != 0 {
        return Err(FixedRefused::Length { field: Field::Pm1aEvent, len: pm1_event_len });
    }
    let gpe0_len = byte(FADT_GPE0_BLK_LEN)?;
    let gpe0 = address(fadt, Field::Gpe0, FADT_GPE0_BLK, FADT_X_GPE0_BLK)?;
    let gpe0 = match (gpe0, gpe0_len) {
        (0, _) => Block::NONE,
        (_, len) if len == 0 || len % 2 != 0 => return Err(FixedRefused::Length { field: Field::Gpe0, len }),
        (address, len) => block(Field::Gpe0, address, len)?,
    };

    let smi_cmd = u32_at(FADT_SMI_CMD)?;
    let smi_cmd = u16::try_from(smi_cmd).map_err(|_| FixedRefused::SmiCmd(smi_cmd))?;
    let legacy = match (smi_cmd, NonZeroU8::new(byte(FADT_ACPI_ENABLE)?), NonZeroU8::new(byte(FADT_ACPI_DISABLE)?)) {
        (1.., Some(acpi_enable), Some(acpi_disable)) => Some(LegacyMode { smi_cmd, acpi_enable, acpi_disable }),
        _ => None,
    };
    Ok(FixedHardware {
        sci_int: fadt.u16_at(FADT_SCI_INT).ok_or(short)?,
        legacy,
        pm1a_event: block(Field::Pm1aEvent, pm1_event, pm1_event_len)?,
        gpe0,
        power_button: if flags & PWR_BUTTON == 0 { PowerButton::Fixed } else { PowerButton::ControlMethod },
    })
}
