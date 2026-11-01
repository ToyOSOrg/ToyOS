//! The `acpi` claim on a hardware-reduced machine (ACPI 6.5 §4.1), which
//! every ACPI Arm machine is: no fixed hardware, no SCI and no legacy mode to
//! leave, so the claim's row names no port and no line.
//!
//! **What the firmware's AML names, this kernel reads and writes for the
//! claim's holder, one access at a time** ([`access`]), with the witness
//! `toyos_userbound::firmware` answers: a function's configuration space
//! through ECAM, and firmware's own memory — ACPI reclaim to read, ACPI NVS
//! both ways — through the direct map, which holds those and RAM and no
//! register. A register's address is refused `NoRegisters`: no range register
//! types one here, and the direct map holds none. There is no port space
//! (`NoPorts`), no Global Lock this kernel exchanges and no sleep type, since
//! PSCI powers the machine off.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use toyos_abi::acpi::{Access, AcpiInfo, Block, Refused, Space, Width, HARDWARE_REDUCED};
use toyos_abi::syscall::SyscallError;
use toyos_acpi::EcamWindow;
use toyos_userbound::firmware::{self, Function as PciFunction, HostBridge, Memory, MemoryVerdict};

use crate::device::ClaimError;
use crate::isa::{self, Function};

/// `isa`'s row for the claim.
const ROW: usize = 1;

struct Hardware {
    rsdp: u64,
    ecam: Vec<EcamWindow>,
}

/// Written once, by [`init`].
static HARDWARE: AtomicPtr<Hardware> = AtomicPtr::new(core::ptr::null_mut());

fn hardware() -> Option<&'static Hardware> {
    let at = HARDWARE.load(Ordering::Acquire);
    // SAFETY: `init` stored a leaked `Box` once, and nothing frees it.
    (!at.is_null()).then(|| unsafe { &*at })
}

/// Fill the claim's row: no port and no line. Before userland runs.
pub fn init(rsdp_addr: u64) {
    isa::fill(ROW, Function { name: "the ACPI tables", runs: vec![], irqs: vec![], wires: vec![] });
    let hardware = Hardware { rsdp: rsdp_addr, ecam: crate::drivers::pci::windows() };
    let was = HARDWARE.swap(alloc::boxed::Box::into_raw(alloc::boxed::Box::new(hardware)), Ordering::Release);
    assert!(was.is_null(), "acpi: init ran twice");
    crate::log!("acpi: the ACPI row: hardware-reduced, its AML's accesses mediated");
}

pub fn claim() -> Result<(isa::Row, AcpiInfo), ClaimError> {
    let hardware = hardware().ok_or(ClaimError::Absent)?;
    let row = isa::claim_row(ROW)?;
    let info = AcpiInfo { rsdp: hardware.rsdp, pm1_event: Block::NONE, gpe0: Block::NONE, flags: HARDWARE_REDUCED, reserved: [0; 3] };
    Ok((row, info))
}

/// Nothing to give back: no mode was entered and no lock taken.
pub fn release() {}

/// No Global Lock this kernel exchanges.
pub fn lock_take(_row: &isa::Row) -> Result<bool, SyscallError> {
    Err(SyscallError::NotSupported)
}

pub fn lock_release(_row: &isa::Row) -> Result<(), SyscallError> {
    Err(SyscallError::NotSupported)
}

/// No sleep type: PSCI powers the machine off.
pub fn s5(_row: &isa::Row, _word: u64) -> Result<(), SyscallError> {
    Err(SyscallError::NotSupported)
}

/// No idle state deeper than the one a CPU waits in is entered here.
pub fn idle(_row: &isa::Row, _declaration: &toyos_abi::acpi::IdleDeclaration) -> Result<(), SyscallError> {
    Err(SyscallError::NotSupported)
}

#[cfg(feature = "test-actuators")]
pub fn debug_firmware_lock(_act: u64) -> u64 {
    SyscallError::NotSupported.to_u64()
}

/// Make the access `request` names for the claim's holder, or refuse it by
/// name, as the x86 claim's `access` does.
pub fn access(_row: &isa::Row, request: &mut Access) -> Result<(), SyscallError> {
    let hardware = hardware().expect("a claimed row has its hardware");
    let (Some(space), Some(width)) = (Space::from_raw(request.space), Width::from_raw(request.width)) else {
        return Err(SyscallError::InvalidArgument);
    };
    let write = match request.write {
        0 => None,
        1 if request.value <= width.max_value() => Some(request.value),
        _ => return Err(SyscallError::InvalidArgument),
    };
    if request.reserved != [0; 3] {
        return Err(SyscallError::InvalidArgument);
    }
    if crate::quiesce::begun() {
        return Err(SyscallError::Gone);
    }
    request.memory_type = toyos_abi::acpi::UNLISTED;
    let made = match space {
        Space::SystemMemory => memory(hardware, request, width, write),
        Space::SystemIo => Err(Refused::NoPorts),
        Space::PciConfig => {
            let at = request.address;
            let function = PciFunction { bus: (at >> 24) as u8, device: (at >> 19 & 0x1F) as u8, function: (at >> 16 & 7) as u8 };
            match at >> 48 {
                0 => config(hardware, (at >> 32) as u16, function, at as u16, width, write),
                _ => Err(Refused::ConfigUnreachable),
            }
        }
    };
    match made {
        Ok(value) => {
            request.refused = 0;
            if write.is_none() {
                request.value = value;
            }
        }
        Err(refused) => request.refused = refused as u8,
    }
    Ok(())
}

/// Bytes at an address whatever its alignment, moved by one access.
#[repr(C, packed)]
struct Unaligned<T>(T);

fn memory(hardware: &Hardware, request: &mut Access, width: Width, write: Option<u64>) -> Result<u64, Refused> {
    let map = crate::mm::firmware_map();
    request.memory_type = firmware::type_word(map, request.address);
    let at = request.address;
    let verdict = crate::pcidev::with_bar_memory(|bars| {
        let memory = Memory {
            map,
            mapped_end: crate::mm::direct_map_end().get(),
            ecam: &hardware.ecam,
            devices: bars,
            facs: None,
            // No register is passed: see the module header.
            uncached: |_, _| false,
            registers_differ: false,
            host_bridge: HostBridge::ABSENT,
        };
        memory.decide(at, width, write.is_some())
    });
    let passed = match verdict {
        MemoryVerdict::AsConfig(function, offset) => return config(hardware, toyos_acpi::SEGMENT_GROUP, function, offset, width, write),
        MemoryVerdict::Refused(Refused::RegisterCached) => return Err(Refused::NoRegisters),
        MemoryVerdict::Refused(refused) => return Err(refused),
        MemoryVerdict::Through(passed) => passed,
    };
    // The policy passed firmware's memory of one type; the direct map holds
    // only the types the kernel reads as memory.
    let mapped = map.iter().find(|e| (e.start..e.end).contains(&at)).is_some_and(|e| toyos_bootmap::is_read_as_memory(e.uefi_type));
    if !mapped {
        return Err(Refused::MemoryType);
    }
    let base = crate::mm::DirectMap::from_phys(passed.at());
    // SAFETY: firmware's own memory, which the direct map holds for the
    // machine's life and no Rust object occupies; the policy passed it, a
    // write only where firmware's map lets a holder write.
    unsafe {
        match (passed.width(), write) {
            (Width::Byte, None) => return Ok(u64::from(base.as_ptr::<u8>().read_volatile())),
            (Width::Word, None) => return Ok(u64::from(base.as_ptr::<Unaligned<u16>>().read_volatile().0)),
            (Width::DWord, None) => return Ok(u64::from(base.as_ptr::<Unaligned<u32>>().read_volatile().0)),
            (Width::QWord, None) => return Ok(base.as_ptr::<Unaligned<u64>>().read_volatile().0),
            (Width::Byte, Some(v)) => base.as_mut_ptr::<u8>().write_volatile(v as u8),
            (Width::Word, Some(v)) => base.as_mut_ptr::<Unaligned<u16>>().write_volatile(Unaligned(v as u16)),
            (Width::DWord, Some(v)) => base.as_mut_ptr::<Unaligned<u32>>().write_volatile(Unaligned(v as u32)),
            (Width::QWord, Some(v)) => base.as_mut_ptr::<Unaligned<u64>>().write_volatile(Unaligned(v)),
        }
    }
    Ok(0)
}

/// One configuration access through the MCFG's window, as the x86 claim's
/// `config` makes it.
fn config(hardware: &Hardware, segment: u16, function: PciFunction, offset: u16, width: Width, write: Option<u64>) -> Result<u64, Refused> {
    let at = firmware::config(&hardware.ecam, segment, function, offset, width)?;
    let PciFunction { bus, device, function } = at.function();
    let space = crate::drivers::pci::function_window(bus, device, function).expect("a function the MCFG's window reaches is mapped");
    let Some(value) = write else {
        let offset = u64::from(at.offset());
        return Ok(match at.width() {
            Width::Byte => u64::from(space.read_u8(offset)),
            Width::Word => u64::from(space.read_u16(offset)),
            Width::DWord => u64::from(space.read_u32(offset)),
            Width::QWord => unreachable!("the policy passes no qword configuration access"),
        });
    };
    let passed = at.write(crate::pcidev::enumerated(bus, device, function))?.at();
    let offset = u64::from(passed.offset());
    match passed.width() {
        Width::Byte => space.write_u8(offset, value as u8),
        Width::Word => space.write_u16(offset, value as u16),
        Width::DWord => space.write_u32(offset, value as u32),
        Width::QWord => unreachable!("the policy passes no qword configuration access"),
    }
    Ok(0)
}
