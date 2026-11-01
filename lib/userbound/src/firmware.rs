//! What the kernel reads and writes for the holder of the `acpi` claim, whose
//! interpreter runs the firmware's AML: one access at a time, to memory, a
//! port or a function's configuration space, each decided here by what the
//! address is.
//!
//! **The holder is trusted platform code, and every check here is a bug
//! fence.** The claim is granted by name in the signed image's manifest, and
//! the firmware whose AML it runs already holds the machine: a write the AML
//! asks for is made unless a fence below refuses it. A fence keeps a write
//! off what the kernel owns: RAM, the tables, the FACS, a device the kernel
//! knows decodes, a function a driver holds, a register that places what the
//! kernel enumerated or declared, and a register the kernel keeps. What no
//! fence sees is recorded in
//! `issues/the-acpi-servers-firmware-writes-reach-itss-and-registers-no-fence-sees.md`.
//!
//! **An access is made only through a witness this module answered**
//! ([`MemoryAt`], [`PortAt`], [`ConfigAt`], [`ConfigWriteAt`], and
//! [`LockWordAt`] for the compare-and-exchange on the FACS): none has another
//! constructor, so a refusal cannot reach an accessor.
//!
//! **Memory is typed by the UEFI memory map** (UEFI 2.10 §7.2,
//! `EFI_MEMORY_TYPE`): what the kernel hands out as RAM is refused whole,
//! ACPI NVS and reserved memory pass both ways, ACPI reclaim memory and
//! runtime-services data are read and never written, an address the map does
//! not list and memory it types `EfiMemoryMappedIO` are accessed where they
//! are a register, and every other type is refused with its type.
//! Runtime-services data is read because some firmware keeps every table its
//! XSDT lists there (UEFI 2.10 §2.3.4 recommends ACPI reclaim); no memory the
//! kernel hands out carries the type (`toyos_bootmap::is_usable_type`).
//!
//! **A register is an address the map does not list or types
//! `EfiMemoryMappedIO`, at or above [`FIXED_RANGE_END`], inside what the
//! kernel maps, that the processor's range registers type uncacheable
//! ([`Memory::uncached`]), on a machine none of whose CPUs holds range
//! registers that are on and differ ([`Memory::registers_differ`]).** That
//! makes one access of it one access on any CPU. It does not keep RAM out,
//! since the range registers are not the effective type everywhere: the
//! allocator hands out only memory the map lists as usable, so memory the map
//! does not list holds nothing ToyOS put there.
//!
//! **Whatever firmware types it, every page a device the kernel knows of
//! decodes in is refused, and a write to a register the kernel keeps
//! ([`HostBridge`]) is refused [`Refused::KernelRegister`].** An address in an
//! ECAM window is a configuration access and is decided as one. What the
//! allocator hands out is refused wherever the map lists it, even where
//! another range types the same byte.
//!
//! **A port is the kernel's to answer where the kernel declared it**
//! ([`crate::port::Mediated`]), another claim's where a row names it, refused
//! where [`PROTECTED`] names it, and passes otherwise, a write as a read.
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
//! **Configuration space is read whole and written past what places the
//! machine** ([`ConfigAt::write`]): never a function a driver holds, an
//! interrupt controller or an IOMMU, any function's header, or a register
//! that places what the kernel enumerated or declared.
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
pub(crate) const EFI_RESERVED: u32 = 0;
const EFI_RUNTIME_DATA: u32 = 6;
const EFI_ACPI_RECLAIM: u32 = 9;
pub(crate) const EFI_ACPI_NVS: u32 = 10;
/// `EfiMemoryMappedIO`.
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

/// One past every function's header, Type 0 and Type 1 alike (PCI Express
/// Base 6.0 §7.5.1): its decode enables and BARs, and a bridge's buses and
/// windows.
const HEADER_END: u16 = 0x40;

/// The host bridges Intel's 11th Generation Core datasheet, volume 2a
/// (631122-003), covers, by device ID: UP3, UP4, UP3-Refresh, H35 and
/// H35-Refresh (631121-012 Table 15-3). Its registers are where the rows
/// below have them on these and no other host bridge.
const MCH_631122: [u16; 5] = [0x9A02, 0x9A04, 0x9A12, 0x9A14, 0x9A1A];

/// Offset 48h's `MCHBAR` (631122 §3.1.14): bits 38:17 the base of a 128 KiB
/// window, bit 0 its enable.
const MCHBAR_BASE: u64 = 0x7F_FFFE_0000;
const MCHBAREN: u64 = 1;

/// The registers in MCHBAR the kernel keeps, each `(first, end)` as an offset
/// into the window (631122):
const KEPT: [(u64, u64); 4] = [
    // §3.4.2 GFXVTBAR and §3.4.4 VTDPVC0BAR: where the two VT-d units decode.
    (0x5400, 0x5408),
    (0x5410, 0x5418),
    // §3.4.5 INTRDIRCTL: how a lowest-priority MSI picks its CPU.
    (0x5418, 0x541C),
    // §3.3.31 PACKAGE_RAPL_LIMIT, PL1 and PL2: the performance track's,
    // which plans to write it; nothing writes it yet
    // (`issues/the-kernel-owns-cpu-performance-state.md`).
    (0x59A0, 0x59A8),
];

/// What the kernel read of 00:00.0 once it enumerated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HostBridge {
    pub vendor: u16,
    pub device: u16,
    /// Its quadword at 48h.
    pub at_48h: u64,
}

impl HostBridge {
    /// Nothing answered at 00:00.0.
    pub const ABSENT: Self = Self { vendor: 0xFFFF, device: 0xFFFF, at_48h: u64::MAX };

    fn covered(vendor: u16, device: u16) -> bool {
        vendor == 0x8086 && MCH_631122.contains(&device)
    }

    /// Where its MCHBAR window is, on a host bridge 631122 covers that
    /// decodes one.
    fn mchbar(&self) -> Option<u64> {
        (Self::covered(self.vendor, self.device) && self.at_48h & MCHBAREN != 0).then_some(self.at_48h & MCHBAR_BASE)
    }

    /// Whether `first..=last` overlaps a register of [`KEPT`].
    fn keeps(&self, first: u64, last: u64) -> bool {
        self.mchbar().is_some_and(|base| KEPT.iter().any(|&(start, end)| overlaps(first, last, (base + start, base + end))))
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
    /// The windows configuration space is reached through, as the MCFG
    /// bounds them.
    pub ecam: &'a [EcamWindow],
    pub devices: D,
    /// The FACS, as `(start, end)`.
    pub facs: Option<(u64, u64)>,
    /// Whether the processor accesses `len` bytes at an address uncached,
    /// whatever maps them: asked only of a register's access, at an address
    /// the map does not list or types `EfiMemoryMappedIO`, at or above
    /// [`FIXED_RANGE_END`].
    pub uncached: fn(u64, u64) -> bool,
    /// Some CPU's range registers are on and are not the ones `uncached`
    /// answers from: what it answers is not known of an access that CPU
    /// makes.
    pub registers_differ: bool,
    pub host_bridge: HostBridge,
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
        if write && self.host_bridge.keeps(at, last) {
            return No(Refused::KernelRegister);
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
            Some(EFI_RESERVED | EFI_ACPI_NVS | EFI_ACPI_RECLAIM | EFI_RUNTIME_DATA | EFI_MMIO) | None => {}
            Some(_) => return No(Refused::MemoryType),
        }
        if last >= self.mapped_end {
            return No(Refused::Unmapped);
        }
        let register = matches!(ty, None | Some(EFI_MMIO));
        if register && self.registers_differ {
            return No(Refused::RangeRegistersDiffer);
        }
        if register && (at < FIXED_RANGE_END || !(self.uncached)(at, width.bytes())) {
            return No(Refused::RegisterCached);
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

/// The ports kept from AML that no declaration of the kernel's holds, each
/// `(first, last)`: the 8259 pair's edge/level control, ELCR1 and ELCR2,
/// which set each of the pair's lines edge- or level-triggered. Intel lists
/// them among the pair's own registers (Intel 332691-003, the 100 Series PCH
/// datasheet, Vol. 2, §28.1, Table 28-1: 4D0h and 4D1h beside 20h-21h and
/// A0h-A1h); the kernel declares and drives the pair, so it is the kernel's
/// whole, though the declaration spans only its command and data ports.
pub const PROTECTED: [(u16, u16); 1] = [(0x4D0, 0x4D1)];

/// Decide an access of `width` at `port`, a write of `write`: every port it
/// spans is asked of `standing` and of [`PROTECTED`].
pub fn port(standing: impl Fn(u16) -> Standing, port: u16, width: Width, write: Option<u64>) -> PortVerdict {
    use PortVerdict::Refused as No;
    if width == Width::QWord || port as usize + width.bytes() as usize > IO_PORTS {
        return No(Refused::PortSpan);
    }
    let last = port + (width.bytes() as u16 - 1);
    if PROTECTED.iter().any(|&(first, end)| port <= end && first <= last) {
        return No(Refused::ProtectedPort);
    }
    let mut commanded: Option<KeptCommands> = None;
    for port in port..=last {
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

/// A configuration access the policy passed the shape of: a read, or a
/// write still to be decided by [`ConfigAt::write`].
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

    /// Decide a write here, where the kernel enumerated `enumerated`, or
    /// nothing. A write is refused [`Refused::HeldFunction`] anywhere in a
    /// function a driver holds, an interrupt controller or an IOMMU (PCI Code
    /// and ID Assignment Specification, base class 08h, subclasses 00h and
    /// 06h): its configuration is its driver's, or the kernel's whether or
    /// not it programs it; and [`Refused::AddressMap`] in any function's
    /// header and in a register [`placing`] names.
    pub fn write(self, enumerated: Option<Enumerated>) -> Result<ConfigWriteAt, Refused> {
        if enumerated.is_some_and(|found| found.held || found.class == 0x08 && matches!(found.subclass, 0x00 | 0x06)) {
            return Err(Refused::HeldFunction);
        }
        let last = self.offset + (self.width.bytes() as u16 - 1);
        if self.offset < HEADER_END || enumerated.is_some_and(|found| placing(found).any(|(first, end)| self.offset < end && first <= last)) {
            return Err(Refused::AddressMap);
        }
        Ok(ConfigWriteAt { at: self })
    }
}

/// A configuration write the policy passed.
///
/// ```compile_fail,E0451
/// fn forged(at: toyos_userbound::firmware::ConfigAt) {
///     let _ = toyos_userbound::firmware::ConfigWriteAt { at };
/// }
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConfigWriteAt {
    at: ConfigAt,
}

impl ConfigWriteAt {
    pub const fn at(&self) -> ConfigAt {
        self.at
    }
}

/// What the kernel enumerated at one function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Enumerated {
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    /// A kernel driver drives it or a claim holds it.
    pub held: bool,
}

/// The registers past the header of a function of this identity that place
/// what the kernel enumerated or declared, each `(first, end)`.
fn placing(found: Enumerated) -> impl Iterator<Item = (u16, u16)> {
    let Enumerated { vendor, device, .. } = found;
    // 631122 §3.1.13–§3.1.34: where the egress port, MCHBAR, graphics
    // memory, the ECAM window, DMIBAR, the legacy ranges and the top of each
    // RAM range are.
    let host_bridge = HostBridge::covered(vendor, device).then_some((0x40, 0xC0));
    // An eSPI controller of the 500 Series PCH (631119-007 Table 6:
    // A080h–A09Fh), whose ESPI_IOD_IOE (631120-002 §2.1.6) enables the decode
    // of COM1 and the i8042's ports.
    let espi = (vendor == 0x8086 && (0xA080..=0xA09F).contains(&device)).then_some((0x80, 0x84));
    // Where a chipset the watchdog knows places its TCO block, and turns it on.
    let tco = toyos_tco::chipset(vendor, device).into_iter().flat_map(|row| [(row.base_reg, row.base_reg + 4), (row.enable.reg, row.enable.reg + 4)]);
    host_bridge.into_iter().chain(espi).chain(tco)
}

/// Decide a configuration access's shape: a function the window reaches, and
/// at most a dword that crosses no dword boundary, the unit a configuration
/// register is defined in.
pub fn config(ecam: &[EcamWindow], segment: u16, function: Function, offset: u16, width: Width) -> Result<ConfigAt, Refused> {
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
