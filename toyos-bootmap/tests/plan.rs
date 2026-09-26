//! The machines this decision is made for: a framebuffer inside the low map,
//! one above it, one on the boundary between them, and one that fits no map;
//! and a loader firmware put below the map's end or above it.

use toyos_bootmap::{Cache, Plan, Refusal, BOOT_MAP_BYTES, MAX_PAGES, PAGE_2M};

const GIB: u64 = 1 << 30;
/// 1920x1080x4, which is neither a whole page nor a whole GiB.
const PANEL: u64 = 0x7e9000;
/// The loader as OVMF loads it on a 2 GiB guest: relocated low, and not on a page.
const LOADER: (u64, u64) = (0x7e5f_0000, 0x3_5000);

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
    let plan = Plan::new(None, LOADER).expect("the low map alone");
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
    let plan = Plan::new(Some((0xc000_0000, PANEL)), LOADER).expect("inside the low map");
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
    let plan = Plan::new(Some((256 * GIB, PANEL)), LOADER).expect("above the low map");
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
    let plan = Plan::new(Some((BOOT_MAP_BYTES, PANEL)), LOADER).expect("at the boundary");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 4]);
    assert!(plan.entries().filter(|e| e.cache == Cache::Uncacheable).all(|e| e.directory == 4));
    is_consistent(&plan);

    // One page below it is the low map's last page, and adds nothing.
    let inside = Plan::new(Some((BOOT_MAP_BYTES - PAGE_2M, PAGE_2M)), LOADER).expect("the last page");
    assert_eq!(inside.directories(), [0, 1, 2, 3]);
    is_consistent(&inside);
}

/// A range that straddles the low map's own end: its first page retypes an
/// entry the low map already holds and the rest are emitted beside it, so both
/// arms of `entries` contribute to one scanout.
#[test]
fn a_framebuffer_that_straddles_the_low_maps_end_is_mapped_from_both_arms() {
    let base = BOOT_MAP_BYTES - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), LOADER).expect("across the end");
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
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), LOADER).expect("straddling");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 7, 8]);
    is_consistent(&plan);

    // With a loader straddling another, the budget is spent to the page.
    let loader = (12 * GIB - 0x1000, 0x3_5000);
    let full = Plan::new(Some((base, 4 * PAGE_2M)), loader).expect("both straddling");
    assert_eq!(full.directories(), [0, 1, 2, 3, 7, 8, 11, 12]);
    assert_eq!(3 + full.directories().len(), MAX_PAGES);
    is_consistent(&full);
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
    assert_eq!(Plan::new(Some((0xc000_1000, PANEL)), LOADER), Err(Refusal::Unaligned(0xc000_1000)));
    // The refusal names the address, because a machine owner reads it.
    assert!(Refusal::Unaligned(0xc000_1000).to_string().contains("0xc0001000"));
}

#[test]
fn a_range_no_map_can_hold_is_refused_by_name() {
    // Past the two PDPTs' 512 GiB.
    assert_eq!(Plan::new(Some((512 * GIB, PANEL)), LOADER), Err(Refusal::PastPdpt(512)));
    // Its own end does not fit an address, either as it is given...
    let base = u64::MAX - PAGE_2M + 1;
    assert_eq!(Plan::new(Some((base, u64::MAX)), LOADER), Err(Refusal::Extent { base, len: u64::MAX }));
    // ...or once rounded up to the page it must end on.
    assert_eq!(Plan::new(Some((base, 1)), LOADER), Err(Refusal::Extent { base, len: 1 }));
    // Wider than the directories a plan may name, and told what it would need.
    assert_eq!(Plan::new(Some((16 * GIB, 10 * GIB)), LOADER), Err(Refusal::Directories(14)));
    assert!(Refusal::Directories(14).to_string().contains("14 page directories"));
}

/// Where lld-link's default `ImageBase` puts the loader when firmware can honour
/// it, 5 GiB on a 4 GiB q35 guest: past the low map, so the `mov cr3` would
/// fetch its next instruction from nowhere unless the map adds it.
#[test]
fn a_loader_above_the_low_map_adds_its_own_directory_of_plain_memory() {
    let loader = (0x1_4000_0000, 0x3_4a00);
    let plan = Plan::new(None, loader).expect("above the low map");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 5]);
    assert_eq!(plan.loader(), (0x1_4000_0000, PAGE_2M));
    let mine: Vec<(u64, usize, Cache)> = plan
        .entries()
        .filter(|e| e.phys >= BOOT_MAP_BYTES)
        .map(|e| (e.phys, e.directory, e.cache))
        .collect();
    assert_eq!(mine, [(0x1_4000_0000, 4, Cache::DeferToMtrr)]);
    is_consistent(&plan);
}

/// A loader inside the low map is already mapped, and adds nothing.
#[test]
fn a_loader_inside_the_low_map_adds_nothing() {
    let plan = Plan::new(None, LOADER).expect("inside");
    assert_eq!(plan.directories(), [0, 1, 2, 3]);
    assert_eq!(plan.entries().count() as u64, BOOT_MAP_BYTES / PAGE_2M);
    assert_eq!(plan.loader(), (0x7e40_0000, 2 * PAGE_2M));
}

/// A loader off the page is rounded out both ways, and one that crosses a page
/// is both pages: its code may run from either.
#[test]
fn a_loader_across_a_page_is_both_pages() {
    let loader = (6 * GIB + PAGE_2M - 0x1000, 0x3_4a00);
    let plan = Plan::new(None, loader).expect("across a page");
    assert_eq!(plan.loader(), (6 * GIB, 2 * PAGE_2M));
    let mine: Vec<u64> = plan.entries().filter(|e| e.phys >= BOOT_MAP_BYTES).map(|e| e.phys).collect();
    assert_eq!(mine, [6 * GIB, 6 * GIB + PAGE_2M]);
    is_consistent(&plan);
}

/// A loader sharing a GiB with the scanout shares its directory, and a page the
/// scanout holds is not written twice.
#[test]
fn a_loader_beside_the_scanout_shares_its_directory() {
    let plan = Plan::new(Some((256 * GIB, PANEL)), (256 * GIB + 0x80_0000, 0x3_4a00))
        .expect("one GiB for both");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 256]);
    let above: Vec<(u64, Cache)> =
        plan.entries().filter(|e| e.phys >= BOOT_MAP_BYTES).map(|e| (e.phys, e.cache)).collect();
    assert_eq!(
        above,
        [
            (256 * GIB, Cache::Uncacheable),
            (256 * GIB + PAGE_2M, Cache::Uncacheable),
            (256 * GIB + 2 * PAGE_2M, Cache::Uncacheable),
            (256 * GIB + 3 * PAGE_2M, Cache::Uncacheable),
            (256 * GIB + 4 * PAGE_2M, Cache::DeferToMtrr),
        ]
    );
    is_consistent(&plan);
}

#[test]
fn a_loader_no_map_can_hold_is_refused_by_name() {
    assert_eq!(Plan::new(None, (512 * GIB, 0x3_4a00)), Err(Refusal::PastPdpt(512)));
    assert_eq!(Plan::new(None, (0x1_4000_0000, 0)), Err(Refusal::Extent { base: 0x1_4000_0000, len: 0 }));
    // Its directories count against the same budget as the scanout's.
    assert_eq!(
        Plan::new(Some((16 * GIB, 4 * GIB)), (32 * GIB, 0x3_4a00)),
        Err(Refusal::Directories(9))
    );
}
