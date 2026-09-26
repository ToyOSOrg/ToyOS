//! The handoff's bounds: an extent is kept only when it is whole blocks inside
//! one descriptor of the loader's type, and each edge of that is its own test.

use toyos_rootimage::handoff::{held, Descriptor};

const PAGE: u64 = 4096;
const LOADER_DATA: u32 = 2;
const CONVENTIONAL: u32 = 7;

fn d(ty: u32, start: u64, end: u64) -> Descriptor {
    Descriptor { ty, start, end }
}

/// One LoaderData descriptor at 16..32 pages between two others.
fn map() -> [Descriptor; 3] {
    [d(CONVENTIONAL, 0, 16 * PAGE), d(LOADER_DATA, 16 * PAGE, 32 * PAGE), d(CONVENTIONAL, 32 * PAGE, 64 * PAGE)]
}

#[test]
fn an_extent_inside_one_descriptor_is_held() {
    assert_eq!(held(map(), LOADER_DATA, 18 * PAGE, 4 * PAGE, PAGE), Some(18 * PAGE..22 * PAGE));
}

#[test]
fn the_whole_descriptor_is_held() {
    assert_eq!(held(map(), LOADER_DATA, 16 * PAGE, 16 * PAGE, PAGE), Some(16 * PAGE..32 * PAGE));
}

#[test]
fn an_extent_starting_one_page_before_the_descriptor_is_refused() {
    assert_eq!(held(map(), LOADER_DATA, 15 * PAGE, 4 * PAGE, PAGE), None);
}

#[test]
fn an_extent_ending_one_page_past_the_descriptor_is_refused() {
    assert_eq!(held(map(), LOADER_DATA, 29 * PAGE, 4 * PAGE, PAGE), None);
}

#[test]
fn an_extent_straddling_two_adjacent_descriptors_of_the_type_is_refused() {
    let map = [d(LOADER_DATA, 16 * PAGE, 24 * PAGE), d(LOADER_DATA, 24 * PAGE, 32 * PAGE)];
    assert_eq!(held(map, LOADER_DATA, 20 * PAGE, 8 * PAGE, PAGE), None);
}

#[test]
fn an_extent_whose_end_overflows_is_refused() {
    let top = u64::MAX / PAGE * PAGE;
    let map = [d(LOADER_DATA, top - 16 * PAGE, u64::MAX)];
    assert_eq!(held(map, LOADER_DATA, top - PAGE, 2 * PAGE, PAGE), None);
}

#[test]
fn a_length_that_is_not_whole_blocks_is_refused() {
    assert_eq!(held(map(), LOADER_DATA, 18 * PAGE, 4 * PAGE + 512, PAGE), None);
}

#[test]
fn a_start_that_is_not_block_aligned_is_refused() {
    assert_eq!(held(map(), LOADER_DATA, 18 * PAGE + 512, 4 * PAGE, PAGE), None);
}

#[test]
fn an_extent_inside_a_descriptor_of_another_type_is_refused() {
    assert_eq!(held(map(), LOADER_DATA, 2 * PAGE, 4 * PAGE, PAGE), None);
}
