//! What the kernel reads and writes for the holder of the `acpi` claim, whose
//! interpreter runs the firmware's AML: one access at a time, to memory, a
//! port or a function's configuration space, each decided here by what the
//! address is.
//!
//! **An access is made only through a witness this module answered**
//! ([`MemoryAt`], [`PortAt`], [`ConfigAt`], and [`LockWordAt`] for the
//! compare-and-exchange on the FACS): none has another constructor, so a
//! refusal cannot reach an accessor.
//!
//! **Memory is the firmware's or it is refused.** The UEFI memory map types
//! every range (UEFI 2.10 §7.2, `EFI_MEMORY_TYPE`): what the kernel hands out
//! as RAM is refused whole, ACPI NVS and reserved memory pass both ways, ACPI
//! reclaim memory, where the tables are, is read and never written, and every
//! other type, and an address the map does not list, is refused with its
//! type, for whoever reads the holder's log to rule on.
//!
//! **Runtime-services data is read and never written, as the tables'
//! memory is, because on some machines it is.** UEFI 2.10 §2.3.4 has "ACPI
//! Tables loaded at boot time ... contained in memory of type
//! EfiACPIReclaimMemory (recommended) or EfiACPIMemoryNVS", and a firmware
//! that keeps every table its XSDT lists in `EfiRuntimeServicesData` exists
//! all the same; the kernel's own reader reads them there. No memory the
//! kernel hands out carries the type (`toyos_bootmap::is_usable_type`). What
//! the holder reads there beside the tables is whatever else that firmware
//! keeps in it. Runtime-services code is refused both ways. Inside that, every
//! page a device the kernel knows of decodes in is refused, whatever firmware
//! types it and whoever drives the device, and an address in the ECAM window
//! is a configuration access and is decided as one.
//!
//! **A port is the kernel's to answer where the kernel declared it**
//! ([`crate::port::Mediated`]), another claim's where a row names it, and
//! passes otherwise.
//!
//! **Configuration space is read and never written.**

use toyos_abi::acpi::{Refused, Width, UNLISTED};
use toyos_abi::boot::MemoryMapEntry;

use crate::port::{Mediated, IO_PORTS};
use crate::span::PAGE_4K;

/// `EfiReservedMemoryType`, `EfiRuntimeServicesData`, `EfiACPIReclaimMemory`
/// and `EfiACPIMemoryNVS`.
const EFI_RESERVED: u32 = 0;
const EFI_RUNTIME_DATA: u32 = 6;
const EFI_ACPI_RECLAIM: u32 = 9;
const EFI_ACPI_NVS: u32 = 10;

/// The local APIC's registers and the window every interrupt message is
/// addressed to (Intel SDM Vol. 3A §11.4.1 and §11.11.1): the kernel's whether
/// or not it maps them.
const LOCAL_APIC: (u64, u64) = (0xFEE0_0000, 0xFEF0_0000);

/// Bytes of configuration space a function has.
const CONFIG_BYTES: u16 = 0x1000;

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

/// What the kernel knows of the machine's memory. `D` yields every range a
/// device decodes that the kernel keeps a record of, as `(start, end)`: each
/// window it mapped to drive one, and each memory BAR of each function.
#[derive(Clone)]
pub struct Memory<'a, D> {
    /// Firmware's map, as the loader handed it over.
    pub map: &'a [MemoryMapEntry],
    /// One past the last byte the kernel maps.
    pub mapped_end: u64,
    pub ecam: Option<Ecam>,
    pub devices: D,
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

/// `range` grown to the pages it lies in. A device decodes whole pages at
/// least, whatever span of them its driver asked to be mapped: an I/O APIC is
/// driven through its first 0x20 bytes, and a chipset's keeps an EOI register
/// further into the same page.
fn pages((start, end): (u64, u64)) -> (u64, u64) {
    (start & !(PAGE_4K - 1), end.saturating_add(PAGE_4K - 1) & !(PAGE_4K - 1))
}

/// The UEFI type of the range of `map` holding `at`, or `None` where it lists
/// none.
fn type_of(map: &[MemoryMapEntry], at: u64) -> Option<u32> {
    map.iter().find(|entry| (entry.start..entry.end).contains(&at)).map(|entry| entry.uefi_type)
}

/// [`type_of`] as [`toyos_abi::acpi::Access::memory_type`] carries it.
pub fn type_word(map: &[MemoryMapEntry], at: u64) -> u8 {
    type_of(map, at).and_then(|ty| u8::try_from(ty).ok()).filter(|&ty| ty != UNLISTED).unwrap_or(UNLISTED)
}

impl<D: IntoIterator<Item = (u64, u64)>> Memory<'_, D> {
    pub fn decide(self, at: u64, width: Width, write: bool) -> MemoryVerdict {
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
        if overlaps(at, last, LOCAL_APIC) || self.devices.into_iter().any(|device| overlaps(at, last, pages(device))) {
            return No(Refused::DeviceMemory);
        }
        let ty = type_of(self.map, at);
        if type_of(self.map, last) != ty {
            return No(Refused::Straddles);
        }
        match ty {
            Some(ty) if toyos_bootmap::is_usable_type(ty) => return No(Refused::UsableMemory),
            Some(EFI_ACPI_RECLAIM | EFI_RUNTIME_DATA) if write => return No(Refused::TableWrite),
            Some(EFI_RESERVED | EFI_ACPI_NVS | EFI_ACPI_RECLAIM | EFI_RUNTIME_DATA) => {}
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

/// The Global Lock's word, where the kernel may exchange it.
///
/// ```compile_fail,E0451
/// let _ = toyos_userbound::firmware::LockWordAt { at: 0x1000 };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LockWordAt {
    at: u64,
}

impl LockWordAt {
    pub const fn at(&self) -> u64 {
        self.at
    }
}

/// Why the lock word a FACS names is none the kernel exchanges.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoLockWord {
    /// A dword not on a dword boundary is no operand of an atomic exchange.
    Misaligned,
    /// One of its four bytes is in memory of this type, or `None` in memory
    /// firmware's map does not list: not firmware's own to keep a lock in, so
    /// an exchange there would write RAM, the tables or a device.
    Type(Option<u32>),
    /// Past the end of what the kernel maps.
    Unmapped,
}

/// Decide whether the dword at `at` is one the kernel exchanges as the Global
/// Lock: all four bytes in memory firmware keeps as its own and a holder may
/// write, ACPI NVS or reserved, and inside what the kernel maps. Each byte is
/// typed, not the first of the structure: a FACS at the end of a firmware
/// range puts the word in whatever follows.
pub fn lock_word(map: &[MemoryMapEntry], mapped_end: u64, at: u64) -> Result<LockWordAt, NoLockWord> {
    if !at.is_multiple_of(4) {
        return Err(NoLockWord::Misaligned);
    }
    // No overflow: `at` is on a dword boundary.
    let last = at + 3;
    for byte in at..=last {
        match type_of(map, byte) {
            Some(EFI_RESERVED | EFI_ACPI_NVS) => {}
            other => return Err(NoLockWord::Type(other)),
        }
    }
    if last >= mapped_end {
        return Err(NoLockWord::Unmapped);
    }
    Ok(LockWordAt { at })
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

/// A configuration read the policy passed.
///
/// ```compile_fail,E0451
/// fn forged(function: toyos_userbound::firmware::Function) {
///     let _ = toyos_userbound::firmware::ConfigAt { function, offset: 0, width: toyos_abi::acpi::Width::Byte };
/// }
/// ```
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

/// Decide a configuration access. A write is refused, whatever it names: a
/// function's configuration space holds what the kernel reads its own
/// configuration from and what moves the ports it declared. A read is held to
/// its shape: a function the window reaches, and at most a dword that crosses
/// no dword boundary, the unit a configuration register is defined in.
pub fn config(ecam: Option<Ecam>, segment: u16, function: Function, offset: u16, width: Width, write: bool) -> Result<ConfigAt, Refused> {
    if write {
        return Err(Refused::ConfigWrite);
    }
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
