//! How far the direct map reaches: to the end of the memory the kernel reads,
//! never to a range the map describes and does not call memory.

use toyos_abi::boot::MemoryMapEntry;
use toyos_memmap::{direct_map_end, is_usable_type, DIRECT_MAP_FLOOR, EFI_LOADER_DATA, PAGE_2M};

const RESERVED: u32 = 0;
const CONVENTIONAL: u32 = 7;
const ACPI_RECLAIM: u32 = 9;
const ACPI_NVS: u32 = 10;
const MMIO: u32 = 11;

const GIB: u64 = 1 << 30;

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

/// The map edk2 hands a 2 GiB q35 guest with an AMD vCPU of 40 physical
/// address bits: the kernel's image and the RSDP where that boot put them, the
/// flash as runtime MMIO, and QEMU's HyperTransport reservation below 1 TiB as
/// reserved memory.
const EDK2_Q35_AMD: [MemoryMapEntry; 9] = [
    e(CONVENTIONAL, 0, 0xa_0000),
    e(CONVENTIONAL, 0x10_0000, 0x7be0_0000),
    e(EFI_LOADER_DATA, 0x7be0_0000, 0x7ca2_c000),
    e(CONVENTIONAL, 0x7ca2_c000, 0x7f77_e000),
    e(ACPI_RECLAIM, 0x7f77_e000, 0x7f77_f000),
    e(ACPI_NVS, 0x7f77_f000, 0x7f80_0000),
    e(RESERVED, 0x7f80_0000, 0x8000_0000),
    e(MMIO, 0xffc0_0000, 0x1_0000_0000),
    e(RESERVED, 0xfd_0000_0000, 0x100_0000_0000),
];

#[test]
fn the_reserved_hole_below_1_tib_is_not_mapped() {
    assert_eq!(
        direct_map_end(&EDK2_Q35_AMD),
        DIRECT_MAP_FLOOR,
        "the direct map reaches past the low 4 GiB for a range the map calls reserved"
    );
}

#[test]
fn memory_above_4_gib_is_mapped_to_its_end_in_whole_pages() {
    let map = [
        e(CONVENTIONAL, 0x10_0000, 0x8000_0000),
        e(CONVENTIONAL, 4 * GIB, 6 * GIB),
        e(EFI_LOADER_DATA, 6 * GIB, 6 * GIB + 0x1000),
    ];
    assert_eq!(direct_map_end(&map), 6 * GIB + PAGE_2M);
}

#[test]
fn acpi_tables_above_the_last_ram_are_mapped() {
    for ty in [ACPI_RECLAIM, ACPI_NVS] {
        let map = [e(CONVENTIONAL, 0, 2 * GIB), e(ty, 8 * GIB, 8 * GIB + 0x3000)];
        assert_eq!(direct_map_end(&map), 8 * GIB + PAGE_2M, "type {ty}");
    }
}

/// The containment the PMM rests on: it touches every frame it hands out
/// through the direct map.
#[test]
fn every_type_the_pmm_hands_out_is_mapped() {
    for ty in (0..=15).filter(|&ty| is_usable_type(ty)) {
        let map = [e(ty, 8 * GIB, 8 * GIB + PAGE_2M)];
        assert_eq!(direct_map_end(&map), 8 * GIB + PAGE_2M, "type {ty}");
    }
}

#[test]
fn no_other_type_reaches_past_the_floor() {
    let others = [0, 5, 6, 8, 11, 12, 13, 14, 15, 0x7000_0000, 0x8000_0000, u32::MAX];
    for ty in others {
        let map = [e(ty, 8 * GIB, 16 * GIB)];
        assert_eq!(direct_map_end(&map), DIRECT_MAP_FLOOR, "type {ty:#x}");
    }
}

#[test]
fn an_empty_map_is_the_floor() {
    assert_eq!(direct_map_end(&[]), DIRECT_MAP_FLOOR);
}
