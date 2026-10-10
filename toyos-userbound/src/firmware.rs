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
//! keeps in it. Runtime-services code is refused both ways.
//!
//! **An address the map does not list is read where it is a register, and
//! never written.** A chipset keeps registers at addresses its firmware lists
//! nowhere, and a machine's AML reads them as it loads. Such a read passes at
//! or above [`FIXED_RANGE_END`] where the kernel maps the address and the
//! processor's range registers type it uncacheable ([`Memory::uncached`]):
//! that is what makes a read of a register one read of it, on a machine
//! whose CPUs all read it under those registers; where some CPU's are on
//! and are not those ([`Memory::registers_differ`]) no such read passes.
//! It is not what
//! keeps RAM out, since the range registers are not the effective type
//! everywhere: the allocator hands out only memory the map lists as usable
//! ([`toyos_bootmap::is_usable_type`]), so memory the map does not list holds
//! nothing ToyOS put there. The read may have an effect in the device that
//! nothing here knows of. Inside that, every page a device the kernel knows
//! of decodes in is refused, whatever firmware types it and whoever drives
//! the device, and an address in an ECAM window is a configuration access
//! and is decided as one.
//!
//! **What the allocator hands out is refused wherever the map lists it.** A
//! map's ranges may overlap, and the allocator takes every usable one: an
//! access is refused where any usable range holds a byte of it, whatever
//! another range types the same byte.
//!
//! **A port is the kernel's to answer where the kernel declared it**
//! ([`crate::port::Mediated`]), another claim's where a row names it, and
//! passes otherwise.
//!
//! **A byte written to the port that commands the firmware is no port write:
//! it is a call into the firmware, the kernel's to make** ([`FirmwareCall`]).
//! What it does there nothing here can bound; this bounds which byte and how
//! often. A byte the machine's tables give a meaning is the kernel's own
//! command and is refused, and of every other the kernel makes [`CALLS`] in
//! any [`CALL_PERIOD_NS`] ([`CallRate`]): firmware's AML retries a call its
//! handler has not answered, and each call stops every CPU of the machine
//! for as long as that handler takes.
//!
//! **Configuration space is read and never written.**
//!
//! **The sleep type of the power-off is the holder's to supply and the
//! kernel's to write** ([`SleepType`]): `\_S5`'s `SLP_TYPa` is in the
//! firmware's AML, which only the holder evaluates, and it is written to a
//! register the holder may not write. A word wider than the register's field
//! is refused, never masked.

use toyos_abi::acpi::{Refused, Width, UNLISTED};
use toyos_abi::boot::MemoryMapEntry;
use toyos_acpi::{ConfigRegister, EcamWindow, SEGMENT_GROUP};

use crate::port::{KeptCommands, Mediated, IO_PORTS};
use crate::span::PAGE_4K;

/// `EfiReservedMemoryType`, `EfiRuntimeServicesData`, `EfiACPIReclaimMemory`
/// and `EfiACPIMemoryNVS`.
const EFI_RESERVED: u32 = 0;
const EFI_RUNTIME_DATA: u32 = 6;
const EFI_ACPI_RECLAIM: u32 = 9;
const EFI_ACPI_NVS: u32 = 10;
/// YOGA HACK: EfiMemoryMappedIO, passed to the AML in the measurement image only.
const EFI_MMIO: u32 = 11;

/// The local APIC's registers and the window every interrupt message is
/// addressed to (Intel SDM Vol. 3A §11.4.1 and §11.11.1): the kernel's whether
/// or not it maps them.
const LOCAL_APIC: (u64, u64) = (0xFEE0_0000, 0xFEF0_0000);

/// One past what the processor's fixed range registers type (Intel SDM
/// Vol. 3A, "Fixed Range MTRRs"). Below it they decide whether a read is
/// cached and the kernel reads none of them, so no address the map does not
/// list is a register's there.
pub const FIXED_RANGE_END: u64 = 0x10_0000;

/// Bytes of configuration space a function has.
const CONFIG_BYTES: u16 = 0x1000;

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
    /// The windows configuration space is reached through, as the MCFG
    /// bounds them.
    pub ecam: &'a [EcamWindow],
    pub devices: D,
    /// The FACS, as `(start, end)`.
    pub facs: Option<(u64, u64)>,
    /// Whether the processor reads `len` bytes at an address uncached,
    /// whatever maps them: asked only of an address the map does not list, at
    /// or above [`FIXED_RANGE_END`].
    pub uncached: fn(u64, u64) -> bool,
    /// Some CPU's range registers are on and are not the ones `uncached`
    /// answers from: what it answers is not known of a read that CPU makes.
    pub registers_differ: bool,
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

/// The type of a range of `map` the allocator hands out that holds a byte of
/// `first..=last`, whichever range [`type_of`] finds first there.
fn usable(map: &[MemoryMapEntry], first: u64, last: u64) -> Option<u32> {
    map.iter()
        .find(|entry| toyos_bootmap::is_usable_type(entry.uefi_type) && overlaps(first, last, (entry.start, entry.end)))
        .map(|entry| entry.uefi_type)
}

impl<D: IntoIterator<Item = (u64, u64)>> Memory<'_, D> {
    pub fn decide(self, at: u64, width: Width, write: bool) -> MemoryVerdict {
        use MemoryVerdict::Refused as No;
        let Some(last) = at.checked_add(width.bytes() - 1) else { return No(Refused::Unmapped) };
        for window in self.ecam {
            match (window.locate(at), window.locate(last)) {
                (Some(register), Some(_)) => {
                    let ConfigRegister { bus, device, function, offset } = register;
                    return MemoryVerdict::AsConfig(Function { bus, device, function }, offset);
                }
                (None, None) => {}
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
        if usable(self.map, at, last).is_some() {
            return No(Refused::UsableMemory);
        }
        match ty {
            Some(EFI_ACPI_RECLAIM | EFI_RUNTIME_DATA) if write => return No(Refused::TableWrite),
            Some(EFI_RESERVED | EFI_ACPI_NVS | EFI_ACPI_RECLAIM | EFI_RUNTIME_DATA) => {}
            // YOGA HACK: the firmware's MMIO, read and written, where the range registers make it uncached.
            Some(EFI_MMIO) => {
                if self.registers_differ {
                    return No(Refused::RangeRegistersDiffer);
                }
                if !(self.uncached)(at, width.bytes()) {
                    return No(Refused::UnlistedCached);
                }
            }
            None if !write => {}
            Some(_) | None => return No(Refused::MemoryType),
        }
        if last >= self.mapped_end {
            return No(Refused::Unmapped);
        }
        if ty.is_none() && self.registers_differ {
            return No(Refused::RangeRegistersDiffer);
        }
        if ty.is_none() && (at < FIXED_RANGE_END || !(self.uncached)(at, width.bytes())) {
            return No(Refused::UnlistedCached);
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
    if let Some(handed_out) = usable(map, at, last) {
        return Err(NoLockWord::Type(Some(handed_out)));
    }
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

/// A command to the firmware the policy passed.
///
/// ```compile_fail,E0451
/// let _ = toyos_userbound::firmware::FirmwareCall { value: 0x10 };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FirmwareCall {
    value: u8,
}

impl FirmwareCall {
    pub const fn value(&self) -> u8 {
        self.value
    }
}

/// What a port access is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortVerdict {
    Through(PortAt),
    /// A byte for the port that commands the firmware: the kernel's to
    /// write, where and as often as it writes one.
    FirmwareCall(FirmwareCall),
    Refused(Refused),
}

/// Decide an access of `width` at `port`, a write of `write`: every port it
/// spans is asked of `standing`.
pub fn port(standing: impl Fn(u16) -> Standing, port: u16, width: Width, write: Option<u64>) -> PortVerdict {
    use PortVerdict::Refused as No;
    if width == Width::QWord || port as usize + width.bytes() as usize > IO_PORTS {
        return No(Refused::PortSpan);
    }
    let mut commanded: Option<KeptCommands> = None;
    for port in port..=port + (width.bytes() as u16 - 1) {
        match standing(port) {
            Standing::Free | Standing::Declared(Mediated::Open) => {}
            Standing::Declared(Mediated::ReadOnly | Mediated::Command(_)) if write.is_none() => {}
            Standing::Declared(Mediated::ReadOnly) => return No(Refused::ReadOnlyPort),
            Standing::Declared(Mediated::Command(kept)) => commanded = Some(kept),
            Standing::Declared(Mediated::Kept) => return No(Refused::KernelPort),
            Standing::Row => return No(Refused::ClaimedPort),
        }
    }
    match (commanded, write) {
        (Some(kept), Some(value)) => match u8::try_from(value) {
            Ok(value) if width == Width::Byte => {
                if kept.holds(value) {
                    No(Refused::KernelCommand)
                } else {
                    PortVerdict::FirmwareCall(FirmwareCall { value })
                }
            }
            _ => No(Refused::CommandSpan),
        },
        _ => PortVerdict::Through(PortAt { port, width }),
    }
}

/// The most commands to the firmware the kernel writes for the holder in any
/// [`CALL_PERIOD_NS`]. The kernel's own number, held against no measurement
/// of a machine's calls: the most one evaluation of the one machine's AML
/// that was read made is two, and its retry of an unanswered call is one a
/// millisecond.
pub const CALLS: usize = 8;
pub const CALL_PERIOD_NS: u64 = 1_000_000_000;

/// When the last [`CALLS`] commands were admitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CallRate {
    admitted: [Option<u64>; CALLS],
    /// The oldest of them, which the next admission replaces.
    next: usize,
}

impl Default for CallRate {
    fn default() -> Self {
        Self::new()
    }
}

impl CallRate {
    pub const fn new() -> Self {
        Self { admitted: [None; CALLS], next: 0 }
    }

    /// Admit a command at `now`, nanoseconds on a clock that does not go
    /// back; or refuse it, and keep nothing of it, where [`CALLS`] were
    /// admitted less than [`CALL_PERIOD_NS`] before it.
    pub fn admit(&mut self, now: u64) -> bool {
        if self.admitted[self.next].is_some_and(|oldest| now.saturating_sub(oldest) < CALL_PERIOD_NS) {
            return false;
        }
        self.admitted[self.next] = Some(now);
        self.next = (self.next + 1) % CALLS;
        true
    }
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
pub fn config(ecam: &[EcamWindow], segment: u16, function: Function, offset: u16, width: Width, write: bool) -> Result<ConfigAt, Refused> {
    if write {
        return Err(Refused::ConfigWrite);
    }
    let Function { bus, device, function: number } = function;
    let reached = segment == SEGMENT_GROUP && ecam.iter().any(|window| window.offset(bus, device, number).is_some());
    if !reached {
        return Err(Refused::ConfigUnreachable);
    }
    let bytes = width.bytes() as u16;
    if width == Width::QWord || offset % 4 + bytes > 4 || offset >= CONFIG_BYTES {
        return Err(Refused::ConfigSpan);
    }
    Ok(ConfigAt { function, offset, width })
}

/// A `SLP_TYPx` the PM1 control register holds (ACPI 6.5 Table 4.16: three
/// bits, 12:10), as the claim's holder supplied it for the power-off.
///
/// ```compile_fail,E0451
/// let _ = toyos_userbound::firmware::SleepType { slp_typ: 5 };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SleepType {
    slp_typ: u8,
}

impl SleepType {
    /// The widest value the field holds.
    const MAX: u64 = 7;
    const SHIFT: u16 = 10;
    /// PM1 control's `SLP_TYPx` field.
    pub const FIELD: u16 = (Self::MAX as u16) << Self::SHIFT;

    pub const fn get(self) -> u8 {
        self.slp_typ
    }

    /// `control` with its `SLP_TYPx` field holding this type and every other
    /// bit as it was.
    pub const fn in_control(self, control: u16) -> u16 {
        control & !Self::FIELD | (self.slp_typ as u16) << Self::SHIFT
    }
}

/// The sleep type `word` names, or none where it is wider than the field:
/// shifted into place such a word would set `SLP_EN` and the reserved bits
/// above it.
pub const fn sleep_type(word: u64) -> Option<SleepType> {
    if word > SleepType::MAX {
        return None;
    }
    Some(SleepType { slp_typ: word as u8 })
}
