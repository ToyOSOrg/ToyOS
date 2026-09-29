//! Which pages the AArch64 kernel's direct map holds: every 4 KiB page
//! firmware's map calls memory the kernel reads, and no other.

use toyos_abi::boot::MemoryMapEntry;
use toyos_bootmap::aarch64::{coverage, direct_map_end, Coverage};
use toyos_bootmap::{DirectMapEnd, Refusal, DIRECT_MAP_WINDOW, PAGE_2M};

const RESERVED: u32 = 0;
const LOADER_DATA: u32 = 2;
const RUNTIME_DATA: u32 = 6;
const CONVENTIONAL: u32 = 7;
const ACPI_RECLAIM: u32 = 9;
const MMIO: u32 = 11;

const GIB: u64 = 1 << 30;

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

fn end(map: &[MemoryMapEntry]) -> Result<u64, Refusal> {
    direct_map_end(map).map(DirectMapEnd::get)
}

/// RAM from 1 GiB, as `virt` has it, with firmware's carve-outs inside it.
const VIRT: [MemoryMapEntry; 7] = [
    e(MMIO, 0x0900_0000, 0x0900_1000),
    e(CONVENTIONAL, GIB, GIB + 0x1F_F000),
    e(RESERVED, GIB + 0x1F_F000, GIB + 0x20_0000),
    e(CONVENTIONAL, GIB + 0x20_0000, GIB + 0x60_0000),
    e(RUNTIME_DATA, GIB + 0x60_0000, GIB + 0x60_3000),
    e(ACPI_RECLAIM, GIB + 0x60_3000, GIB + 0x60_4000),
    e(LOADER_DATA, GIB + 0x60_4000, 3 * GIB),
];

#[test]
fn memory_is_mapped_and_a_register_is_not() {
    assert_eq!(end(&VIRT), Ok(3 * GIB));
    assert_eq!(coverage(&VIRT, 0x0900_0000 & !(PAGE_2M - 1)), Coverage::Nothing);
    assert_eq!(coverage(&VIRT, 0), Coverage::Nothing);
}

#[test]
fn a_page_wholly_memory_is_one_block() {
    assert_eq!(coverage(&VIRT, GIB + PAGE_2M), Coverage::Whole);
    assert_eq!(coverage(&VIRT, 2 * GIB), Coverage::Whole);
}

#[test]
fn a_reserved_page_inside_ram_is_left_out_of_its_block() {
    let first = coverage(&VIRT, GIB);
    assert!(matches!(first, Coverage::Pages(_)));
    assert!(first.holds(0));
    assert!(first.holds(510));
    assert!(!first.holds(511), "EfiReservedMemoryType is not memory the kernel reads");
}

#[test]
fn runtime_services_data_is_left_out_and_acpi_tables_are_not() {
    let page = coverage(&VIRT, GIB + 3 * PAGE_2M);
    for runtime in 0..3 {
        assert!(!page.holds(runtime), "runtime services data, page {runtime}");
    }
    assert!(page.holds(3), "the ACPI table page");
    assert!(page.holds(4), "loader data");
    assert!(page.holds(511), "loader data");
}

#[test]
fn off_a_page_is_refused() {
    let map = [e(CONVENTIONAL, GIB, GIB + 0x800)];
    assert_eq!(end(&map), Err(Refusal::OffPage(GIB + 0x800)));
}

#[test]
fn past_the_window_is_refused() {
    let map = [e(CONVENTIONAL, DIRECT_MAP_WINDOW, DIRECT_MAP_WINDOW + PAGE_2M)];
    assert_eq!(end(&map), Err(Refusal::PastWindow(DIRECT_MAP_WINDOW + PAGE_2M)));
}

#[test]
fn a_map_with_no_memory_ends_at_zero() {
    assert_eq!(end(&[e(MMIO, 0x0900_0000, 0x0900_1000)]), Ok(0));
}

#[test]
fn a_start_off_a_page_is_refused() {
    let map = [e(CONVENTIONAL, GIB + 0x800, GIB + PAGE_2M)];
    assert_eq!(end(&map), Err(Refusal::OffPage(GIB + 0x800)));
}

#[test]
fn an_end_below_its_start_is_refused() {
    let map = [e(CONVENTIONAL, GIB + PAGE_2M, GIB)];
    assert_eq!(end(&map), Err(Refusal::Extent { base: GIB + PAGE_2M, len: GIB.wrapping_sub(GIB + PAGE_2M) }));
}
