//! The handoff map's relabel: the claimed part gets the claim's type, the rest
//! keeps its own, and nothing is lost, doubled or reordered.

use toyos_bootmap::relabel::{relabel, Extent};

const LOADER_DATA: u32 = 2;
const CONVENTIONAL: u32 = 7;
const ROOT: u32 = 0x8000_7201;

fn e(start: u64, end: u64, ty: u32) -> Extent {
    Extent { start, end, ty }
}

fn pieces(extent: Extent, claim: Extent) -> Vec<Extent> {
    relabel(extent, LOADER_DATA, claim).into_iter().flatten().collect()
}

#[test]
fn a_claim_inside_a_descriptor_splits_it_in_three() {
    assert_eq!(
        pieces(e(0x1000, 0x9000, LOADER_DATA), e(0x3000, 0x5000, ROOT)),
        [e(0x1000, 0x3000, LOADER_DATA), e(0x3000, 0x5000, ROOT), e(0x5000, 0x9000, LOADER_DATA)]
    );
}

#[test]
fn a_claim_flush_with_an_edge_leaves_no_empty_piece() {
    assert_eq!(pieces(e(0x1000, 0x9000, LOADER_DATA), e(0x1000, 0x9000, ROOT)), [e(0x1000, 0x9000, ROOT)]);
    assert_eq!(
        pieces(e(0x1000, 0x9000, LOADER_DATA), e(0x1000, 0x4000, ROOT)),
        [e(0x1000, 0x4000, ROOT), e(0x4000, 0x9000, LOADER_DATA)]
    );
    assert_eq!(
        pieces(e(0x1000, 0x9000, LOADER_DATA), e(0x4000, 0x9000, ROOT)),
        [e(0x1000, 0x4000, LOADER_DATA), e(0x4000, 0x9000, ROOT)]
    );
}

/// Two adjacent descriptors the claim straddles: each gives up only its own
/// part, and the kernel's check then sees two pieces, which it refuses as it
/// should.
#[test]
fn a_claim_across_two_descriptors_relabels_each_part() {
    let claim = e(0x3000, 0x7000, ROOT);
    assert_eq!(pieces(e(0x1000, 0x5000, LOADER_DATA), claim), [e(0x1000, 0x3000, LOADER_DATA), e(0x3000, 0x5000, ROOT)]);
    assert_eq!(pieces(e(0x5000, 0x9000, LOADER_DATA), claim), [e(0x5000, 0x7000, ROOT), e(0x7000, 0x9000, LOADER_DATA)]);
}

#[test]
fn what_the_claim_misses_or_another_type_holds_is_returned_whole() {
    let claim = e(0x3000, 0x5000, ROOT);
    for extent in [
        e(0x0000, 0x3000, LOADER_DATA),
        e(0x5000, 0x9000, LOADER_DATA),
        e(0x1000, 0x9000, CONVENTIONAL),
        e(0x4000, 0x4000, LOADER_DATA),
    ] {
        assert_eq!(pieces(extent, claim), [extent]);
    }
    // No claim at all: a boot the loader handed no image.
    assert_eq!(pieces(e(0x1000, 0x9000, LOADER_DATA), e(0, 0, ROOT)), [e(0x1000, 0x9000, LOADER_DATA)]);
}
