//! The machines this decision is made for: a framebuffer inside the low map,
//! one above it, one on the boundary between them, and one that fits no map.

use toyos_bootmap::{Cache, Plan, Refusal, BOOT_MAP_BYTES, MAX_DIRECTORIES, MAX_PAGES, PAGE_2M};

const GIB: u64 = 1 << 30;
/// 1920x1080x4, which is neither a whole page nor a whole GiB.
const PANEL: u64 = 0x7e9000;

/// Every entry lands in a directory the plan named, at an index inside it, and
/// no two entries land in the same place.
fn is_consistent(plan: &Plan) {
    let mut seen: Vec<(usize, usize)> = Vec::new();
    for entry in plan.entries() {
        assert!(entry.directory < plan.directories().len(), "{entry:?}");
        assert!(entry.index < 512, "{entry:?}");
        assert_eq!(
            plan.directories()[entry.directory],
            entry.phys / GIB,
            "{entry:?} is in another GiB's directory"
        );
        assert_eq!(entry.index as u64, (entry.phys / PAGE_2M) % 512, "{entry:?}");
        assert!(seen.iter().all(|at| *at != (entry.directory, entry.index)), "{entry:?} twice");
        seen.push((entry.directory, entry.index));
    }
    assert_eq!(plan.pages(), 3 + plan.directories().len());
    assert!(plan.pages() <= MAX_PAGES);
}

#[test]
fn a_machine_with_no_framebuffer_is_the_low_map_and_nothing_else() {
    let plan = Plan::new(None).expect("the low map alone");
    assert_eq!(plan.directories(), [0, 1, 2, 3]);
    assert_eq!(plan.scanout(), None);
    assert_eq!(plan.entries().count() as u64, BOOT_MAP_BYTES / PAGE_2M);
    assert!(plan.entries().all(|e| e.cache == Cache::DeferToMtrr));
    is_consistent(&plan);
}

/// QEMU's, at 3 GiB: inside the low map, so it adds no directory and only
/// retypes pages the low map already holds.
#[test]
fn a_framebuffer_inside_the_low_map_adds_no_directory() {
    let plan = Plan::new(Some((0xc000_0000, PANEL))).expect("inside the low map");
    assert_eq!(plan.directories(), [0, 1, 2, 3]);
    assert_eq!(plan.pages(), 7);
    // Rounded up to whole pages, and up only.
    assert_eq!(plan.scanout(), Some((0xc000_0000, 0x80_0000)));
    let uncacheable: Vec<u64> =
        plan.entries().filter(|e| e.cache == Cache::Uncacheable).map(|e| e.phys).collect();
    assert_eq!(uncacheable, [0xc000_0000, 0xc020_0000, 0xc040_0000, 0xc060_0000]);
    is_consistent(&plan);
}

/// The T14's, at 256 GiB: one new directory, reached through both views, with
/// the low map untouched.
#[test]
fn a_framebuffer_above_the_low_map_adds_its_own_directory() {
    let plan = Plan::new(Some((256 * GIB, PANEL))).expect("above the low map");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 256]);
    assert_eq!(plan.pages(), 8);
    assert_eq!(plan.scanout(), Some((256 * GIB, 0x80_0000)));
    let mine: Vec<usize> = plan
        .entries()
        .filter(|e| e.cache == Cache::Uncacheable)
        .map(|e| {
            assert_eq!(e.directory, 4, "the scanout is not in the low map's directories");
            e.index
        })
        .collect();
    assert_eq!(mine, [0, 1, 2, 3]);
    is_consistent(&plan);
}

/// The boundary: a framebuffer that begins where the low map ends is one GiB
/// past it, not the last GiB of it.
#[test]
fn a_framebuffer_at_the_boundary_is_outside_the_low_map() {
    let plan = Plan::new(Some((BOOT_MAP_BYTES, PANEL))).expect("at the boundary");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 4]);
    assert!(plan.entries().filter(|e| e.cache == Cache::Uncacheable).all(|e| e.directory == 4));
    is_consistent(&plan);

    // One page below it is the low map's last page, and adds nothing.
    let inside = Plan::new(Some((BOOT_MAP_BYTES - PAGE_2M, PAGE_2M))).expect("the last page");
    assert_eq!(inside.directories(), [0, 1, 2, 3]);
    is_consistent(&inside);
}

/// A range that straddles a GiB is two directories, and the second is claimed
/// once however many pages fall in it.
#[test]
fn a_framebuffer_that_straddles_a_gib_claims_both() {
    let base = 8 * GIB - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M))).expect("straddling");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 7, 8]);
    assert_eq!(plan.pages(), MAX_PAGES);
    is_consistent(&plan);
}

#[test]
fn a_base_off_the_page_is_refused_rather_than_rounded_down() {
    assert_eq!(Plan::new(Some((0xc000_1000, PANEL))), Err(Refusal::Unaligned(0xc000_1000)));
    // The refusal names the address, because a machine owner reads it.
    assert!(Refusal::Unaligned(0xc000_1000).to_string().contains("0xc0001000"));
}

#[test]
fn a_range_no_map_can_hold_is_refused_by_name() {
    // Past the two PDPTs' 512 GiB.
    assert_eq!(Plan::new(Some((512 * GIB, PANEL))), Err(Refusal::PastPdpt(512)));
    // Its own end does not fit an address.
    let base = u64::MAX - PAGE_2M + 1;
    assert_eq!(Plan::new(Some((base, u64::MAX))), Err(Refusal::Extent { base, len: u64::MAX }));
    // Wider than the directories a plan may name: the low map's four and two.
    assert_eq!(Plan::new(Some((16 * GIB, 3 * GIB))), Err(Refusal::Directories(MAX_DIRECTORIES + 1)));
}
