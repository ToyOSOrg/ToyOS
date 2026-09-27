//! How far the direct map reaches: to the end of the memory the kernel reads,
//! never to a range the map describes and does not call memory; and what a
//! firmware address the kernel reads through it must lie inside.

use toyos_abi::boot::MemoryMapEntry;
use toyos_bootmap::x86_64::direct_map_end;
use toyos_bootmap::{
    is_usable_type, reaches, Refusal, BOOT_MAP_BYTES, DIRECT_MAP_WINDOW, EFI_LOADER_DATA, PAGE_2M,
};

const RESERVED: u32 = 0;
const CONVENTIONAL: u32 = 7;
const ACPI_RECLAIM: u32 = 9;
const ACPI_NVS: u32 = 10;
const MMIO: u32 = 11;

const GIB: u64 = 1 << 30;

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

/// 2 GiB of memory, and a reserved range just below 1 TiB.
const RESERVED_HOLE: [MemoryMapEntry; 9] = [
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
        direct_map_end(&RESERVED_HOLE),
        Ok(BOOT_MAP_BYTES),
        "the direct map reaches past the boot map for a range the map calls reserved"
    );
}

#[test]
fn an_unsorted_map_is_read_whole() {
    let map = [
        e(RESERVED, 0xfd_0000_0000, 0x100_0000_0000),
        e(CONVENTIONAL, 4 * GIB, 8 * GIB),
        e(CONVENTIONAL, 0x10_0000, 2 * GIB),
    ];
    assert_eq!(direct_map_end(&map), Ok(8 * GIB), "the highest memory is neither first nor last");
}

#[test]
fn memory_above_4_gib_is_mapped_to_its_end_in_whole_pages() {
    let map = [
        e(CONVENTIONAL, 0x10_0000, 0x8000_0000),
        e(CONVENTIONAL, 4 * GIB, 6 * GIB),
        e(EFI_LOADER_DATA, 6 * GIB, 6 * GIB + 0x1000),
    ];
    assert_eq!(direct_map_end(&map), Ok(6 * GIB + PAGE_2M));
}

#[test]
fn acpi_tables_above_the_last_ram_are_mapped() {
    for ty in [ACPI_RECLAIM, ACPI_NVS] {
        let map = [e(CONVENTIONAL, 0, 2 * GIB), e(ty, 8 * GIB, 8 * GIB + 0x3000)];
        assert_eq!(direct_map_end(&map), Ok(8 * GIB + PAGE_2M), "type {ty}");
    }
}

/// UEFI §7.2's table: loader code and data, boot services code and data, and
/// conventional memory are the OS's after `ExitBootServices`, and nothing else is.
#[test]
fn the_pmm_hands_out_exactly_the_types_uefi_gives_the_os() {
    let usable: Vec<u32> = (0..=255)
        .chain([0x7000_0000, 0x7fff_ffff, 0x8000_0000, u32::MAX])
        .filter(|&ty| is_usable_type(ty))
        .collect();
    assert_eq!(usable, [1, 2, 3, 4, 7]);
}

/// The containment the pmm rests on: it touches every frame it hands out
/// through the direct map.
#[test]
fn every_type_the_pmm_hands_out_is_mapped() {
    for ty in (0..=255).filter(|&ty| is_usable_type(ty)) {
        let map = [e(ty, 8 * GIB, 8 * GIB + PAGE_2M)];
        assert_eq!(direct_map_end(&map), Ok(8 * GIB + PAGE_2M), "type {ty}");
    }
}

#[test]
fn no_other_type_reaches_past_the_boot_map() {
    let others = [0, 5, 6, 8, 11, 12, 13, 14, 15, 0x7000_0000, 0x8000_0000, u32::MAX];
    for ty in others {
        let map = [e(ty, 8 * GIB, 16 * GIB)];
        assert_eq!(direct_map_end(&map), Ok(BOOT_MAP_BYTES), "type {ty:#x}");
    }
}

#[test]
fn an_empty_map_is_the_boot_map() {
    assert_eq!(direct_map_end(&[]), Ok(BOOT_MAP_BYTES));
}

/// Root slots 256 to 511 at `PHYS_OFFSET`: everything from there to the top of
/// the address space.
#[test]
fn the_window_is_what_phys_offset_leaves() {
    const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;
    assert_eq!(DIRECT_MAP_WINDOW, 0u64.wrapping_sub(PHYS_OFFSET));
}

#[test]
fn memory_past_the_window_is_refused_by_name() {
    for ty in (0..=255).filter(|&ty| is_usable_type(ty)) {
        let map = [e(CONVENTIONAL, 0x10_0000, 2 * GIB), e(ty, 4 * GIB, u64::MAX)];
        assert_eq!(direct_map_end(&map), Err(Refusal::PastWindow(u64::MAX)), "type {ty}");
    }
    let map = [e(CONVENTIONAL, DIRECT_MAP_WINDOW - PAGE_2M, DIRECT_MAP_WINDOW + 1)];
    assert_eq!(direct_map_end(&map), Err(Refusal::PastWindow(DIRECT_MAP_WINDOW + 1)));
    let map = [e(CONVENTIONAL, DIRECT_MAP_WINDOW - PAGE_2M, DIRECT_MAP_WINDOW)];
    assert_eq!(direct_map_end(&map), Ok(DIRECT_MAP_WINDOW));
}

#[test]
fn a_range_past_the_window_that_is_not_memory_is_not_refused() {
    let map = [e(CONVENTIONAL, 0x10_0000, 2 * GIB), e(RESERVED, 0, u64::MAX)];
    assert_eq!(direct_map_end(&map), Ok(BOOT_MAP_BYTES));
}

/// What the ACPI reader asks of a firmware address: the direct map's extent,
/// not the architecture's physical-address width.
#[test]
fn a_firmware_address_past_the_direct_map_is_not_read() {
    let end = direct_map_end(&RESERVED_HOLE).unwrap();
    assert!(!reaches(end, 0xfd_0000_0000, 36), "a table in the reserved hole");
    assert!(!reaches(BOOT_MAP_BYTES, 1 << 40, 1), "before the kernel's own map, past the boot map");
    assert!(reaches(end, end - 36, 36));
    assert!(!reaches(end, end - 35, 36), "a table whose last byte is past the map");
    assert!(!reaches(end, u64::MAX, 2), "a range whose end does not fit an address");
}
