//! Which memory-mapped address space this platform declared, and which of it
//! nothing owns.
//!
//! **The firmware's account is read; the address space is never probed.** A
//! load at an address no bridge forwards does not answer all-ones on this
//! hardware — it does not complete, and the CPU cannot be interrupted out of
//! it: a ThinkPad T14 issued one at `0xd0000000` and was gone for the 420 s the
//! metal loop waits, through the lockup detector and the boot deadline alike.
//! So nothing here or in the kernel reads an address whose routing is unknown.
//!
//! The Global Coherency Domain is DXE's own map of the physical address space
//! (PI 1.8 Vol. 2 §7.2). EDK2's `PciHostBridgeDxe` adds each root bridge's
//! Mem, PMem and MemAbove4G apertures to it as
//! [`MEMORY_MAPPED_IO`][`GcdType::MEMORY_MAPPED_IO`] and allocates every BAR it
//! assigns out of them, so an MMIO descriptor with no owner is aperture the
//! bridges decode and nothing has taken. That is the necessary condition
//! `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL`'s `Configuration()` cannot give: it
//! answers a bridge's *current* settings, which a BAR may lie outside.
//!
//! It has to be asked here, like the root bridge protocol: DXE services die
//! with boot services.

use core::ffi::c_void;

use toyos_abi::boot::RootBridgeWindow;
use uefi::prelude::*;
use uefi::{guid, Guid};

const HEAD: &str = "GCD:";

/// The configuration table entry carrying [`DxeServices`] (PI 1.8 Vol. 2 §7.1).
const DXE_SERVICES_TABLE_GUID: Guid = guid!("05ad34ba-6f02-4214-952e-4da0398e2bb9");

/// `EFI_DXE_SERVICES_TABLE_SIGNATURE`: the eight bytes `DXE_SERV`, read as a
/// little-endian word. Checked because a configuration table entry is a pointer
/// firmware wrote and this loader calls through it.
const SIGNATURE: u64 = u64::from_le_bytes(*b"DXE_SERV");

/// `EFI_GCD_MEMORY_TYPE` (PI 1.8 Vol. 2 §7.2.1), which the descriptor carries
/// as a C enum and therefore as four bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GcdType(u32);

impl GcdType {
    const NON_EXISTENT: Self = Self(0);
    const RESERVED: Self = Self(1);
    const SYSTEM_MEMORY: Self = Self(2);
    const MEMORY_MAPPED_IO: Self = Self(3);
    const PERSISTENT: Self = Self(4);
    const MORE_RELIABLE: Self = Self(5);

    fn name(self) -> &'static str {
        match self {
            Self::NON_EXISTENT => "nonexistent",
            Self::RESERVED => "reserved",
            Self::SYSTEM_MEMORY => "memory",
            Self::MEMORY_MAPPED_IO => "mmio",
            Self::PERSISTENT => "persistent",
            Self::MORE_RELIABLE => "more-reliable",
            _ => "unknown",
        }
    }
}

/// `EFI_GCD_MEMORY_SPACE_DESCRIPTOR` (PI 1.8 Vol. 2 §7.2.1), in the spec's own
/// field order.
#[repr(C)]
#[derive(Clone, Copy)]
struct MemorySpace {
    base: u64,
    length: u64,
    capabilities: u64,
    attributes: u64,
    kind: GcdType,
    /// The image that allocated this range, and the device it was allocated
    /// for. **Both null is the whole of what "free" means here** (PI 1.8
    /// Vol. 2 §7.2.3: `AllocateMemorySpace` sets them and `FreeMemorySpace`
    /// clears them).
    image: *mut c_void,
    device: *mut c_void,
}

/// The enum is four bytes and the two handles are eight each, so the u32 is
/// followed by four bytes of padding; a struct the compiler laid out otherwise
/// would read every field past `kind` off by that much.
const _: () = assert!(core::mem::size_of::<MemorySpace>() == 56);
const _: () = assert!(core::mem::offset_of!(MemorySpace, image) == 40);

/// `DXE_SERVICES` (PI 1.8 Vol. 2 §7.1), in the spec's own field order. Only
/// `get_memory_space_map` is called; the rest are here because their order is
/// what puts it where it is.
#[repr(C)]
struct DxeServices {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
    add_memory_space: *mut c_void,
    allocate_memory_space: *mut c_void,
    free_memory_space: *mut c_void,
    remove_memory_space: *mut c_void,
    get_memory_space_descriptor: *mut c_void,
    set_memory_space_attributes: *mut c_void,
    get_memory_space_map: unsafe extern "efiapi" fn(
        count: *mut usize,
        map: *mut *mut MemorySpace,
    ) -> Status,
    add_io_space: *mut c_void,
    allocate_io_space: *mut c_void,
    free_io_space: *mut c_void,
    remove_io_space: *mut c_void,
    get_io_space_descriptor: *mut c_void,
    get_io_space_map: *mut c_void,
    dispatch: *mut c_void,
    schedule: *mut c_void,
    trust: *mut c_void,
    process_firmware_volume: *mut c_void,
    set_memory_space_capabilities: *mut c_void,
}

/// A field added or dropped above moves `get_memory_space_map`, and the symptom
/// is firmware being asked to add memory space instead of describing it: the
/// 24-byte table header and eighteen pointers.
const _: () = assert!(core::mem::size_of::<DxeServices>() == 24 + 18 * 8);
const _: () = assert!(core::mem::offset_of!(DxeServices, get_memory_space_map) == 24 + 6 * 8);

/// The smallest run worth handing the kernel: the only page it maps.
const GRANULE: u64 = 2 * 1024 * 1024;

/// Append every memory-mapped range this platform declared and nothing owns to
/// `out`, and answer how many were added.
///
/// Every descriptor is logged, owned or not, because that log is the only
/// account of this machine's address space that leaves it — and the one a
/// reading of the kernel's choice is checked against.
pub fn free_mmio(system_table: &SystemTable<Boot>, out: &mut [RootBridgeWindow]) -> usize {
    let Some(entry) =
        system_table.config_table().iter().find(|e| e.guid == DXE_SERVICES_TABLE_GUID)
    else {
        println!("{HEAD} no configuration table carries DXE services, so none of this machine's declared address space is known");
        return 0;
    };
    // SAFETY: the entry firmware installed under the DXE services GUID points
    // at that table for the life of boot services, and the signature below is
    // what says the pointer is the table rather than a coincidence of GUIDs.
    let services = unsafe { &*(entry.address as *const DxeServices) };
    if services.signature != SIGNATURE {
        println!(
            "{HEAD} the DXE services table signature is {:#018x} and not {SIGNATURE:#018x}, so it is not called",
            services.signature
        );
        return 0;
    }

    let mut count: usize = 0;
    let mut map: *mut MemorySpace = core::ptr::null_mut();
    // SAFETY: the call is the spec's — two out parameters, which firmware fills
    // with a count and a pool buffer it allocated and this loader frees below.
    let status = unsafe { (services.get_memory_space_map)(&mut count, &mut map) };
    if !status.is_success() || map.is_null() {
        println!("{HEAD} GetMemorySpaceMap() answered {status:?}, so none of this machine's declared address space is known");
        return 0;
    }

    let mut added = 0usize;
    for index in 0..count {
        // SAFETY: firmware answered `count` descriptors at `map`, and `index`
        // is inside that count.
        let space = unsafe { *map.add(index) };
        let owner = if space.image.is_null() && space.device.is_null() { "free" } else { "held" };
        println!(
            "{HEAD} {:#x}+{:#x} {} cap={:#x} attr={:#x} {owner}",
            space.base,
            space.length,
            space.kind.name(),
            space.capabilities,
            space.attributes,
        );
        if space.kind != GcdType::MEMORY_MAPPED_IO || owner != "free" || space.length < GRANULE {
            continue;
        }
        let Some(slot) = out.get_mut(added) else {
            // Refused by name and the rest dropped: the kernel is handed what
            // this loader could carry, and a reader is told there was more.
            println!("{HEAD} more free mmio ranges than the {} the kernel is handed", out.len());
            break;
        };
        *slot = RootBridgeWindow { base: space.base, length: space.length };
        added += 1;
    }

    // SAFETY: `map` is the pool buffer `GetMemorySpaceMap` allocated, nothing
    // else holds it, and every descriptor has been copied out of it above.
    let _ = unsafe { system_table.boot_services().free_pool(map as *mut u8) };
    println!("{HEAD} {count} descriptor(s); {added} free mmio range(s) of {GRANULE:#x} bytes or more handed to the kernel");
    added
}
