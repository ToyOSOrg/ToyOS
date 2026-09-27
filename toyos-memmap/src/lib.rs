//! What the kernel takes from firmware's memory map, by UEFI memory type, pure.
//!
//! The types are `EFI_MEMORY_TYPE`'s, and what an OS may do with each after
//! `ExitBootServices` is the UEFI specification's "Memory Type Usage after
//! ExitBootServices()" table (`EFI_BOOT_SERVICES.AllocatePages()`, §7.2). A
//! conforming map may describe any address, memory or not: a range the table
//! calls "not usable" or "not used by the OS" is still in the map, anywhere in
//! the physical address space.

#![no_std]
#![forbid(unsafe_code)]

use toyos_abi::boot::MemoryMapEntry;

const EFI_LOADER_CODE: u32 = 1;
pub const EFI_LOADER_DATA: u32 = 2;
const EFI_BOOT_SERVICES_CODE: u32 = 3;
const EFI_BOOT_SERVICES_DATA: u32 = 4;
const EFI_CONVENTIONAL_MEMORY: u32 = 7;
const EFI_ACPI_RECLAIM_MEMORY: u32 = 9;
const EFI_ACPI_MEMORY_NVS: u32 = 10;

/// The direct map's leaf.
pub const PAGE_2M: u64 = 2 * 1024 * 1024;

/// The low 4 GiB, mapped whatever the map says: the platform's registers and
/// firmware's own tables are there, and neither is described as memory.
pub const DIRECT_MAP_FLOOR: u64 = 4 * 1024 * 1024 * 1024;

/// Whether a UEFI memory type becomes free RAM the PMM will hand out.
pub const fn is_usable_type(uefi_type: u32) -> bool {
    matches!(
        uefi_type,
        EFI_LOADER_CODE
            | EFI_LOADER_DATA
            | EFI_BOOT_SERVICES_CODE
            | EFI_BOOT_SERVICES_DATA
            | EFI_CONVENTIONAL_MEMORY
    )
}

/// Whether the kernel reads a range of this type as memory: what the PMM hands
/// out, and the two types ACPI's tables live in.
///
/// Every other type is left out, a type this list does not know included:
/// reserved and I/O ranges are nothing a write-back mapping may cover, unusable
/// memory is memory with errors, and runtime services code, persistent and
/// unaccepted memory are nothing this kernel reads.
const fn is_read_as_memory(uefi_type: u32) -> bool {
    is_usable_type(uefi_type)
        || matches!(uefi_type, EFI_ACPI_RECLAIM_MEMORY | EFI_ACPI_MEMORY_NVS)
}

/// One past the kernel direct map's last byte: [`DIRECT_MAP_FLOOR`], or the
/// end of the highest range the kernel reads as memory in whole [`PAGE_2M`]
/// pages, whichever is higher.
pub fn direct_map_end(map: &[MemoryMapEntry]) -> u64 {
    map.iter()
        .filter(|entry| is_read_as_memory(entry.uefi_type))
        .map(|entry| entry.end.saturating_add(PAGE_2M - 1) & !(PAGE_2M - 1))
        .fold(DIRECT_MAP_FLOOR, u64::max)
}
