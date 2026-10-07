//! What the kernel reads and writes for the holder of the `acpi` claim, whose
//! interpreter runs the firmware's AML: one access at a time, to memory, a
//! port or a function's configuration space, each decided here by what the
//! address is.
//!
//! **An access is made only through a witness this module answered**
//! ([`MemoryAt`], [`PortAt`], [`ConfigAt`], [`ConfigWrite`]): none has another
//! constructor, so a refusal cannot reach an accessor.
//!
//! **Memory is the firmware's or it is refused.** The UEFI memory map types
//! every range (UEFI 2.10 §7.2, `EFI_MEMORY_TYPE`): what the kernel hands out
//! as RAM is refused whole, ACPI NVS and reserved memory pass both ways, ACPI
//! reclaim memory, where the tables are, is read and never written, and every
//! other type, and an address the map does not list, is refused with its
//! type, for whoever reads the holder's log to rule on. Inside that, a window
//! the kernel drives a device through is refused, and an address in the ECAM
//! window is a configuration access and is decided as one.
//!
//! **A port is the kernel's to answer where the kernel declared it**
//! ([`crate::port::Mediated`]), another claim's where a row names it, and
//! passes otherwise.
//!
//! **Configuration space is read whole and written only where nothing the
//! kernel decides lives**: not in the standard header, not in a capability
//! the kernel programs, not in extended space, and not on a function a driver
//! holds.

use toyos_abi::acpi::{Refused, Width, UNLISTED};
use toyos_abi::boot::MemoryMapEntry;

use crate::port::{Mediated, IO_PORTS};

/// `EfiReservedMemoryType`, `EfiACPIReclaimMemory` and `EfiACPIMemoryNVS`.
const EFI_RESERVED: u32 = 0;
const EFI_ACPI_RECLAIM: u32 = 9;
const EFI_ACPI_NVS: u32 = 10;

/// The local APIC's registers and the window every interrupt message is
/// addressed to (Intel SDM Vol. 3A §11.4.1 and §11.11.1): the kernel's whether
/// or not it maps them.
const LOCAL_APIC: (u64, u64) = (0xFEE0_0000, 0xFEF0_0000);

/// Bytes of configuration space a function has, and of its standard header.
const CONFIG_BYTES: u16 = 0x1000;
const CONFIG_HEADER: u16 = 0x40;
/// Where conventional configuration space ends and extended begins.
const CONFIG_EXTENDED: u16 = 0x100;

/// The window configuration space is reached through, as the MCFG names it
/// (PCI Firmware Specification 3.3, Table 4-3): `base` is bus 0's, whatever
/// the first bus the window decodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ecam {
    pub base: u64,
    pub segment: u16,
    pub first_bus: u8,
    pub last_bus: u8,
}

impl Ecam {
    fn holds(&self, at: u64) -> bool {
        // Saturating: the base is firmware's word.
        let start = self.base.saturating_add(u64::from(self.first_bus) << 20);
        let end = self.base.saturating_add((u64::from(self.last_bus) + 1) << 20);
        (start..end).contains(&at)
    }
}

/// One PCI function on the segment group the kernel enumerated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Function {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// What the kernel knows of the machine's memory.
pub struct Memory<'a> {
    /// Firmware's map, as the loader handed it over.
    pub map: &'a [MemoryMapEntry],
    /// One past the last byte the kernel maps.
    pub mapped_end: u64,
    pub ecam: Option<Ecam>,
    /// Every window the kernel mapped to drive a device, as `(start, end)`.
    pub driven: &'a [(u64, u64)],
    /// The FACS, as `(start, end)`.
    pub facs: Option<(u64, u64)>,
}

/// A memory access the policy passed.
///
/// ```compile_fail,E0451
/// let _ = toyos_userbound::firmware::MemoryAt { at: 0x1000, width: toyos_abi::acpi::Width::Byte };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryAt {
    at: u64,
    width: Width,
}

impl MemoryAt {
    pub const fn at(&self) -> u64 {
        self.at
    }

    pub const fn width(&self) -> Width {
        self.width
    }
}

/// What a memory access is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryVerdict {
    Through(MemoryAt),
    /// In the ECAM window: this function's configuration space at this
    /// offset, still to be decided by [`config`].
    AsConfig(Function, u16),
    Refused(Refused),
}

fn overlaps(first: u64, last: u64, (start, end): (u64, u64)) -> bool {
    first < end && start <= last
}

impl Memory<'_> {
    /// The UEFI type of the range holding `at`, or `None` where the map lists
    /// none.
    pub fn type_of(&self, at: u64) -> Option<u32> {
        self.map.iter().find(|entry| (entry.start..entry.end).contains(&at)).map(|entry| entry.uefi_type)
    }

    /// [`Self::type_of`] as [`toyos_abi::acpi::Access::memory_type`] carries
    /// it.
    pub fn type_word(&self, at: u64) -> u8 {
        self.type_of(at).and_then(|ty| u8::try_from(ty).ok()).filter(|&ty| ty != UNLISTED).unwrap_or(UNLISTED)
    }

    pub fn decide(&self, at: u64, width: Width, write: bool) -> MemoryVerdict {
        use MemoryVerdict::Refused as No;
        let Some(last) = at.checked_add(width.bytes() - 1) else { return No(Refused::Unmapped) };
        if let Some(ecam) = self.ecam {
            match (ecam.holds(at), ecam.holds(last)) {
                (true, true) => {
                    let offset = at - ecam.base;
                    let function =
                        Function { bus: (offset >> 20) as u8, device: (offset >> 15 & 0x1F) as u8, function: (offset >> 12 & 7) as u8 };
                    return MemoryVerdict::AsConfig(function, (offset & 0xFFF) as u16);
                }
                (false, false) => {}
                _ => return No(Refused::Straddles),
            }
        }
        if overlaps(at, last, LOCAL_APIC) || self.driven.iter().any(|&window| overlaps(at, last, window)) {
            return No(Refused::KernelDevice);
        }
        let ty = self.type_of(at);
        if self.type_of(last) != ty {
            return No(Refused::Straddles);
        }
        match ty {
            Some(ty) if toyos_bootmap::is_usable_type(ty) => return No(Refused::UsableMemory),
            Some(EFI_ACPI_RECLAIM) if write => return No(Refused::TableWrite),
            Some(EFI_RESERVED | EFI_ACPI_NVS | EFI_ACPI_RECLAIM) => {}
            Some(_) | None => return No(Refused::MemoryType),
        }
        if last >= self.mapped_end {
            return No(Refused::Unmapped);
        }
        if write && self.facs.is_some_and(|facs| overlaps(at, last, facs)) {
            return No(Refused::FacsWrite);
        }
        MemoryVerdict::Through(MemoryAt { at, width })
    }
}

/// What the kernel says of one port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    /// Nothing declared it and no row names it.
    Free,
    /// The kernel declared it, with this answer.
    Declared(Mediated),
    /// A row another claim is for names it.
    Row,
}

/// A port access the policy passed.
///
/// ```compile_fail,E0451
/// let _ = toyos_userbound::firmware::PortAt { port: 0x3F8, width: toyos_abi::acpi::Width::Byte };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PortAt {
    port: u16,
    width: Width,
}

impl PortAt {
    pub const fn port(&self) -> u16 {
        self.port
    }

    pub const fn width(&self) -> Width {
        self.width
    }
}

/// Decide an access of `width` at `port`: every port it spans is asked of
/// `standing`.
pub fn port(standing: impl Fn(u16) -> Standing, port: u16, width: Width, write: bool) -> Result<PortAt, Refused> {
    if width == Width::QWord || port as usize + width.bytes() as usize > IO_PORTS {
        return Err(Refused::PortSpan);
    }
    for port in port..=port + (width.bytes() as u16 - 1) {
        match standing(port) {
            Standing::Free | Standing::Declared(Mediated::Open) => {}
            Standing::Declared(Mediated::ReadOnly) if !write => {}
            Standing::Declared(Mediated::ReadOnly) => return Err(Refused::ReadOnlyPort),
            Standing::Declared(Mediated::Kept) => return Err(Refused::KernelPort),
            Standing::Row => return Err(Refused::ClaimedPort),
        }
    }
    Ok(PortAt { port, width })
}

/// A configuration access the policy passed as a read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfigAt {
    function: Function,
    offset: u16,
    width: Width,
}

impl ConfigAt {
    pub const fn function(&self) -> Function {
        self.function
    }

    pub const fn offset(&self) -> u16 {
        self.offset
    }

    pub const fn width(&self) -> Width {
        self.width
    }
}

/// Decide the shape of a configuration access, which is all a read is held
/// to: a function the window reaches, and at most a dword that crosses no
/// dword boundary, the unit a configuration register is defined in.
pub fn config(ecam: Option<Ecam>, segment: u16, function: Function, offset: u16, width: Width) -> Result<ConfigAt, Refused> {
    let reached = ecam.is_some_and(|ecam| {
        ecam.segment == segment && (ecam.first_bus..=ecam.last_bus).contains(&function.bus)
    });
    if !reached || function.device > 31 || function.function > 7 {
        return Err(Refused::ConfigUnreachable);
    }
    let bytes = width.bytes() as u16;
    if width == Width::QWord || offset % 4 + bytes > 4 || offset >= CONFIG_BYTES {
        return Err(Refused::ConfigSpan);
    }
    Ok(ConfigAt { function, offset, width })
}

/// A configuration write the policy passed.
///
/// ```compile_fail,E0603
/// fn forged(at: toyos_userbound::firmware::ConfigAt) {
///     let _ = toyos_userbound::firmware::ConfigWrite(at);
/// }
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfigWrite(ConfigAt);

impl ConfigWrite {
    pub const fn at(&self) -> ConfigAt {
        self.0
    }
}

/// Decide a write at `at`. `free` is whether the kernel enumerated the
/// function and neither a kernel driver nor a claim holds it, and `programmed`
/// the capabilities the kernel programs on a function it hands out, each as
/// its offset and length.
pub fn config_write(at: ConfigAt, free: bool, programmed: impl IntoIterator<Item = (u8, u8)>) -> Result<ConfigWrite, Refused> {
    let (first, last) = (at.offset, at.offset + (at.width.bytes() as u16 - 1));
    if first < CONFIG_HEADER {
        return Err(Refused::ConfigHeader);
    }
    if last >= CONFIG_EXTENDED {
        return Err(Refused::ConfigExtended);
    }
    if !free {
        return Err(Refused::ConfigDriven);
    }
    for (offset, len) in programmed {
        if first < u16::from(offset) + u16::from(len) && u16::from(offset) <= last {
            return Err(Refused::ConfigCapability);
        }
    }
    Ok(ConfigWrite(at))
}
