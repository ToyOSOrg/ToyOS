//! The machines this decision is made for: a framebuffer inside the low map,
//! one above it, one on the boundary between them, and one that fits no map.

use toyos_bootmap::{Cache, Plan, Refusal, BOOT_MAP_BYTES, MAX_PAGES, PAGE_2M};

const GIB: u64 = 1 << 30;
/// 1920x1080x4, which is neither a whole page nor a whole GiB.
const PANEL: u64 = 0x7e9000;

/// Every entry lands in a directory the plan named, at an index inside it, and
/// no two entries land in the same place — the property the loader's writes
/// rest on, since it stores each one where the plan says without looking.
fn is_consistent(plan: &Plan) {
    let mut seen: Vec<(usize, usize)> = Vec::new();
    for entry in plan.entries() {
        assert!(entry.directory < plan.directories().len(), "{entry:?}");
        assert!(entry.index < 512, "{entry:?}");
        assert!(seen.iter().all(|at| *at != (entry.directory, entry.index)), "{entry:?} twice");
        seen.push((entry.directory, entry.index));
    }
    assert!(3 + plan.directories().len() <= MAX_PAGES);
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

/// A range that straddles the low map's own end: its first page retypes an
/// entry the low map already holds and the rest are emitted beside it, so both
/// arms of `entries` contribute to one scanout.
#[test]
fn a_framebuffer_that_straddles_the_low_maps_end_is_mapped_from_both_arms() {
    let base = BOOT_MAP_BYTES - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M))).expect("across the end");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 4]);
    let mine: Vec<(u64, usize)> = plan
        .entries()
        .filter(|e| e.cache == Cache::Uncacheable)
        .map(|e| (e.phys, e.directory))
        .collect();
    assert_eq!(
        mine,
        [
            // Retyped in place, in the low map's last directory.
            (base, 3),
            // Emitted, in the directory this range added.
            (BOOT_MAP_BYTES, 4),
            (BOOT_MAP_BYTES + PAGE_2M, 4),
            (BOOT_MAP_BYTES + 2 * PAGE_2M, 4),
        ]
    );
    is_consistent(&plan);
}

/// A range that straddles a GiB is two directories, and the second is claimed
/// once however many pages fall in it.
#[test]
fn a_framebuffer_that_straddles_a_gib_claims_both() {
    let base = 8 * GIB - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M))).expect("straddling");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 7, 8]);
    assert_eq!(3 + plan.directories().len(), MAX_PAGES);
    is_consistent(&plan);
}

/// The bits the loader stores, decided here: PAT entry 3 is PCD and PWT with
/// the PAT bit clear, and entry 0 is none of the three.
#[test]
fn uncacheable_is_pat_entry_three_and_plain_memory_is_entry_zero() {
    const PWT: u64 = 1 << 3;
    const PCD: u64 = 1 << 4;
    const PAT_2M: u64 = 1 << 12;
    assert_eq!(Cache::DeferToMtrr.bits(), 0);
    assert_eq!(Cache::Uncacheable.bits(), PCD | PWT);
    // The PAT bit is what would select entry 4, and nothing here sets it.
    assert_eq!(Cache::Uncacheable.bits() & PAT_2M, 0);
    assert_eq!(Cache::DeferToMtrr.bits() & PAT_2M, 0);
}

/// The two views, so the loader reads the slots rather than knowing them.
#[test]
fn the_high_half_slot_is_the_top_nine_bits_of_phys_offset() {
    const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;
    assert_eq!(toyos_bootmap::PML4_IDENTITY, 0);
    assert_eq!(toyos_bootmap::PML4_HIGH_HALF, ((PHYS_OFFSET >> 39) & 0x1ff) as usize);
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
    // Its own end does not fit an address, either as it is given...
    let base = u64::MAX - PAGE_2M + 1;
    assert_eq!(Plan::new(Some((base, u64::MAX))), Err(Refusal::Extent { base, len: u64::MAX }));
    // ...or once rounded up to the page it must end on.
    assert_eq!(Plan::new(Some((base, 1))), Err(Refusal::Extent { base, len: 1 }));
    // Wider than the directories a plan may name, and told what it would need.
    assert_eq!(Plan::new(Some((16 * GIB, 10 * GIB))), Err(Refusal::Directories(14)));
    assert!(Refusal::Directories(14).to_string().contains("14 page directories"));
}
