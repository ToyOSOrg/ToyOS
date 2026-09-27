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

const GIB: u64 = 1 << 30;

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

/// The map QEMU 11.1.1's edk2 (`edk2-x86_64-code.fd`) hands a 2 GiB q35 guest
/// with `-cpu qemu64`, every descriptor as a boot of this kernel printed it. The
/// last is QEMU's HyperTransport reservation below 1 TiB.
const EDK2_Q35_AMD: [MemoryMapEntry; 127] = [
    e(7, 0x0, 0x87000),
    e(4, 0x87000, 0x88000),
    e(7, 0x88000, 0xa0000),
    e(7, 0x100000, 0x800000),
    e(10, 0x800000, 0x808000),
    e(7, 0x808000, 0x80b000),
    e(10, 0x80b000, 0x80c000),
    e(7, 0x80c000, 0x811000),
    e(10, 0x811000, 0x900000),
    e(4, 0x900000, 0x1780000),
    e(7, 0x1780000, 0x8000000),
    e(2, 0x8000000, 0x8004000),
    e(7, 0x8004000, 0x7753c000),
    e(2, 0x7753c000, 0x7bb3c000),
    e(4, 0x7bb3c000, 0x7bb5c000),
    e(7, 0x7bb5c000, 0x7bc1b000),
    e(2, 0x7bc1b000, 0x7cd93000),
    e(1, 0x7cd93000, 0x7cdea000),
    e(7, 0x7cdea000, 0x7ce11000),
    e(2, 0x7ce11000, 0x7ce14000),
    e(7, 0x7ce14000, 0x7ce15000),
    e(2, 0x7ce15000, 0x7ce24000),
    e(4, 0x7ce24000, 0x7ce25000),
    e(2, 0x7ce25000, 0x7ce27000),
    e(4, 0x7ce27000, 0x7e180000),
    e(2, 0x7e180000, 0x7e183000),
    e(4, 0x7e183000, 0x7e189000),
    e(2, 0x7e189000, 0x7e18a000),
    e(4, 0x7e18a000, 0x7e2b5000),
    e(3, 0x7e2b5000, 0x7e35c000),
    e(4, 0x7e35c000, 0x7e3b4000),
    e(3, 0x7e3b4000, 0x7e4a7000),
    e(4, 0x7e4a7000, 0x7e4cc000),
    e(3, 0x7e4cc000, 0x7e4e1000),
    e(4, 0x7e4e1000, 0x7e4e5000),
    e(3, 0x7e4e5000, 0x7e50e000),
    e(4, 0x7e50e000, 0x7e515000),
    e(3, 0x7e515000, 0x7e524000),
    e(4, 0x7e524000, 0x7e526000),
    e(3, 0x7e526000, 0x7e54a000),
    e(4, 0x7e54a000, 0x7e54b000),
    e(3, 0x7e54b000, 0x7e55f000),
    e(4, 0x7e55f000, 0x7e561000),
    e(3, 0x7e561000, 0x7e567000),
    e(4, 0x7e567000, 0x7e56d000),
    e(3, 0x7e56d000, 0x7e57a000),
    e(4, 0x7e57a000, 0x7e587000),
    e(3, 0x7e587000, 0x7e5b7000),
    e(4, 0x7e5b7000, 0x7e5cb000),
    e(3, 0x7e5cb000, 0x7e5e1000),
    e(4, 0x7e5e1000, 0x7e5e4000),
    e(3, 0x7e5e4000, 0x7e602000),
    e(4, 0x7e602000, 0x7e604000),
    e(3, 0x7e604000, 0x7e62d000),
    e(4, 0x7e62d000, 0x7e638000),
    e(3, 0x7e638000, 0x7e647000),
    e(4, 0x7e647000, 0x7e64f000),
    e(3, 0x7e64f000, 0x7e6a0000),
    e(4, 0x7e6a0000, 0x7e6a6000),
    e(3, 0x7e6a6000, 0x7e6b0000),
    e(4, 0x7e6b0000, 0x7e6b2000),
    e(3, 0x7e6b2000, 0x7e6bf000),
    e(4, 0x7e6bf000, 0x7e6c1000),
    e(3, 0x7e6c1000, 0x7e6e6000),
    e(4, 0x7e6e6000, 0x7e6e8000),
    e(3, 0x7e6e8000, 0x7e6eb000),
    e(4, 0x7e6eb000, 0x7e6ee000),
    e(3, 0x7e6ee000, 0x7e707000),
    e(4, 0x7e707000, 0x7e70c000),
    e(3, 0x7e70c000, 0x7e735000),
    e(4, 0x7e735000, 0x7e736000),
    e(3, 0x7e736000, 0x7e760000),
    e(4, 0x7e760000, 0x7e762000),
    e(3, 0x7e762000, 0x7e766000),
    e(4, 0x7e766000, 0x7e76a000),
    e(3, 0x7e76a000, 0x7e775000),
    e(4, 0x7e775000, 0x7e778000),
    e(3, 0x7e778000, 0x7e77c000),
    e(4, 0x7e77c000, 0x7e780000),
    e(3, 0x7e780000, 0x7e786000),
    e(4, 0x7e786000, 0x7e78d000),
    e(3, 0x7e78d000, 0x7e7e7000),
    e(4, 0x7e7e7000, 0x7e7ec000),
    e(3, 0x7e7ec000, 0x7e7ef000),
    e(4, 0x7e7ef000, 0x7e7f4000),
    e(3, 0x7e7f4000, 0x7e7fa000),
    e(4, 0x7e7fa000, 0x7ea03000),
    e(3, 0x7ea03000, 0x7ea06000),
    e(4, 0x7ea06000, 0x7ea09000),
    e(3, 0x7ea09000, 0x7ea2e000),
    e(6, 0x7ea2e000, 0x7eaef000),
    e(3, 0x7eaef000, 0x7eb17000),
    e(4, 0x7eb17000, 0x7eb1e000),
    e(3, 0x7eb1e000, 0x7eb2d000),
    e(4, 0x7eb2d000, 0x7eb57000),
    e(3, 0x7eb57000, 0x7eb74000),
    e(4, 0x7eb74000, 0x7eb77000),
    e(3, 0x7eb77000, 0x7eb7a000),
    e(4, 0x7eb7a000, 0x7eb7d000),
    e(3, 0x7eb7d000, 0x7eb86000),
    e(4, 0x7eb86000, 0x7eb89000),
    e(3, 0x7eb89000, 0x7eb8c000),
    e(4, 0x7eb8c000, 0x7eb8f000),
    e(3, 0x7eb8f000, 0x7eb90000),
    e(4, 0x7eb90000, 0x7ef91000),
    e(3, 0x7ef91000, 0x7efa2000),
    e(4, 0x7efa2000, 0x7efa6000),
    e(3, 0x7efa6000, 0x7efbd000),
    e(4, 0x7efbd000, 0x7f4ed000),
    e(6, 0x7f4ed000, 0x7f5ed000),
    e(5, 0x7f5ed000, 0x7f6ed000),
    e(0, 0x7f6ed000, 0x7f76d000),
    e(9, 0x7f76d000, 0x7f77f000),
    e(10, 0x7f77f000, 0x7f7ff000),
    e(4, 0x7f7ff000, 0x7fe00000),
    e(7, 0x7fe00000, 0x7fe16000),
    e(4, 0x7fe16000, 0x7fe36000),
    e(3, 0x7fe36000, 0x7fe80000),
    e(0, 0x7fe80000, 0x7fe84000),
    e(10, 0x7fe84000, 0x7fe86000),
    e(3, 0x7fe86000, 0x7fe87000),
    e(4, 0x7fe87000, 0x7feb0000),
    e(3, 0x7feb0000, 0x7fedc000),
    e(6, 0x7fedc000, 0x7ff60000),
    e(10, 0x7ff60000, 0x80000000),
    e(0, 0xe0000000, 0xf0000000),
    e(0, 0xfd00000000, 0x10000000000),
];

#[test]
fn the_reserved_hole_below_1_tib_is_not_mapped() {
    assert_eq!(
        direct_map_end(&EDK2_Q35_AMD),
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

/// That boot's pmm counted 2140844032 usable bytes in 112 entries.
#[test]
fn the_captured_map_is_usable_where_its_boot_said() {
    let usable: Vec<&MemoryMapEntry> = EDK2_Q35_AMD.iter().filter(|e| is_usable_type(e.uefi_type)).collect();
    assert_eq!(usable.len(), 112);
    assert_eq!(usable.iter().map(|e| e.end - e.start).sum::<u64>(), 2_140_844_032);
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
    let map = [e(CONVENTIONAL, DIRECT_MAP_WINDOW - PAGE_2M, DIRECT_MAP_WINDOW + 1)];
    assert_eq!(
        direct_map_end(&map),
        Err(Refusal::PastWindow(DIRECT_MAP_WINDOW + 1)),
        "memory one byte past the window"
    );
    let map = [e(CONVENTIONAL, DIRECT_MAP_WINDOW - PAGE_2M, DIRECT_MAP_WINDOW)];
    assert_eq!(direct_map_end(&map), Ok(DIRECT_MAP_WINDOW), "memory that ends at the window");
    for ty in (0..=255).filter(|&ty| is_usable_type(ty)) {
        let map = [e(CONVENTIONAL, 0x10_0000, 2 * GIB), e(ty, 4 * GIB, u64::MAX)];
        assert_eq!(direct_map_end(&map), Err(Refusal::PastWindow(u64::MAX)), "type {ty}");
    }
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
    let end = direct_map_end(&EDK2_Q35_AMD).unwrap();
    // The RSDP and the five tables the same boot read, where it found them.
    for (at, len) in [
        (0x7f77e014, 36),
        (0x7f778000, 144),
        (0x7f779000, 244),
        (0x7f777000, 56),
        (0x7f776000, 60),
        (0x7f775000, 128),
    ] {
        assert!(reaches(end, at, len), "{at:#x}+{len}");
    }
    assert!(!reaches(end, 0xfd_0000_0000, 36), "a table in the reserved hole");
    assert!(!reaches(BOOT_MAP_BYTES, 1 << 40, 1), "before the kernel's own map, past the boot map");
    assert!(reaches(end, end - 36, 36));
    assert!(!reaches(end, end - 35, 36), "a table whose last byte is past the map");
    assert!(!reaches(end, u64::MAX, 2), "a range whose end does not fit an address");
}
