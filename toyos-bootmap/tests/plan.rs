//! The machines this decision is made for: a framebuffer inside the low map,
//! one above it, one on the boundary between them, and one that fits no map.

use toyos_bootmap::{
    aarch64, x86_64, Cache, Entry, Plan, Refusal, Slot, Typing, BOOT_MAP_BYTES, MAX_PAGES, PAGE_2M,
    PAGE_4K,
};

const GIB: u64 = 1 << 30;
/// 1920x1080x4, which is neither a whole page nor a whole GiB.
const PANEL: u64 = 0x7e9000;

/// Every leaf lands in a table the plan named, at an index inside it, no two
/// land in the same place, and no leaf lands where a split page's table is
/// named — the property the loader's writes rest on, since it stores each one
/// where the plan says without looking.
fn is_consistent(plan: &Plan) {
    let mut seen: Vec<Slot> = Vec::new();
    let tables: Vec<(usize, usize)> = plan.fine_slots().collect();
    for entry in plan.entries() {
        match entry.slot {
            Slot::Directory { directory, index } => {
                assert!(directory < plan.directories().len(), "{entry:?}");
                assert!(index < 512, "{entry:?}");
                assert!(!tables.contains(&(directory, index)), "{entry:?} over a split page's table");
            }
            Slot::Fine { table, index } => {
                assert!(table < plan.fine_tables().len(), "{entry:?}");
                assert!(index < 512, "{entry:?}");
            }
        }
        assert!(!seen.contains(&entry.slot), "{entry:?} twice");
        seen.push(entry.slot);
    }
    assert!(3 + plan.directories().len() + plan.fine_tables().len() <= MAX_PAGES);
}

/// A 2 MiB leaf's directory, by position.
fn directory(e: &Entry) -> usize {
    match e.slot {
        Slot::Directory { directory, .. } => directory,
        Slot::Fine { .. } => panic!("{e:?} is a 4 KiB leaf"),
    }
}

#[test]
fn a_machine_with_no_framebuffer_is_the_low_map_and_nothing_else() {
    let plan = Plan::new(None, Typing::Firmware).expect("the low map alone");
    assert_eq!(plan.directories(), [0, 1, 2, 3]);
    assert_eq!(plan.scanout(), None);
    assert_eq!(plan.entries().count() as u64, BOOT_MAP_BYTES / PAGE_2M);
    assert!(plan.entries().all(|e| e.cache == Cache::Firmware));
    is_consistent(&plan);
}

/// QEMU's, at 3 GiB: inside the low map, so it adds no directory and only
/// retypes pages the low map already holds.
#[test]
fn a_framebuffer_inside_the_low_map_adds_no_directory() {
    let plan = Plan::new(Some((0xc000_0000, PANEL)), Typing::Firmware).expect("inside the low map");
    assert_eq!(plan.directories(), [0, 1, 2, 3]);
    // Rounded up to whole pages, and up only.
    assert_eq!(plan.scanout(), Some((0xc000_0000, 0x80_0000)));
    let uncacheable: Vec<u64> =
        plan.entries().filter(|e| e.cache == Cache::Scanout).map(|e| e.phys).collect();
    assert_eq!(uncacheable, [0xc000_0000, 0xc020_0000, 0xc040_0000, 0xc060_0000]);
    is_consistent(&plan);
}

/// The T14's, at 256 GiB: one new directory, reached through both views, with
/// the low map untouched.
#[test]
fn a_framebuffer_above_the_low_map_adds_its_own_directory() {
    let plan = Plan::new(Some((256 * GIB, PANEL)), Typing::Firmware).expect("above the low map");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 256]);
    assert_eq!(plan.scanout(), Some((256 * GIB, 0x80_0000)));
    let mine: Vec<usize> = plan
        .entries()
        .filter(|e| e.cache == Cache::Scanout)
        .map(|e| {
            let Slot::Directory { directory, index } = e.slot else { panic!("{e:?}") };
            assert_eq!(directory, 4, "the scanout is not in the low map's directories");
            index
        })
        .collect();
    assert_eq!(mine, [0, 1, 2, 3]);
    is_consistent(&plan);
}

/// The boundary: a framebuffer that begins where the low map ends is one GiB
/// past it, not the last GiB of it.
#[test]
fn a_framebuffer_at_the_boundary_is_outside_the_low_map() {
    let plan = Plan::new(Some((BOOT_MAP_BYTES, PANEL)), Typing::Firmware).expect("at the boundary");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 4]);
    assert!(plan.entries().filter(|e| e.cache == Cache::Scanout).all(|e| directory(&e) == 4));
    is_consistent(&plan);

    // One page below it is the low map's last page, and adds nothing.
    let inside = Plan::new(Some((BOOT_MAP_BYTES - PAGE_2M, PAGE_2M)), Typing::Firmware).expect("the last page");
    assert_eq!(inside.directories(), [0, 1, 2, 3]);
    is_consistent(&inside);
}

/// A range that straddles the low map's own end: its first page retypes an
/// entry the low map already holds and the rest are emitted beside it, so both
/// arms of `entries` contribute to one scanout.
#[test]
fn a_framebuffer_that_straddles_the_low_maps_end_is_mapped_from_both_arms() {
    let base = BOOT_MAP_BYTES - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), Typing::Firmware).expect("across the end");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 4]);
    let mine: Vec<(u64, usize)> = plan
        .entries()
        .filter(|e| e.cache == Cache::Scanout)
        .map(|e| (e.phys, directory(&e)))
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
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), Typing::Firmware).expect("straddling");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 7, 8]);
    // Every directory the budget has; the two split-page tables are a
    // map-typed plan's, and this one splits nothing.
    assert_eq!(3 + plan.directories().len() + 2, MAX_PAGES);
    assert!(plan.fine_tables().is_empty());
    is_consistent(&plan);
}


/// The two views, so the loader reads the slots rather than knowing them.
#[test]
fn the_high_half_slot_is_the_top_nine_bits_of_phys_offset() {
    const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;
    assert_eq!(toyos_bootmap::ROOT_IDENTITY, 0);
    assert_eq!(toyos_bootmap::ROOT_HIGH_HALF, ((PHYS_OFFSET >> 39) & 0x1ff) as usize);
}

#[test]
fn a_base_off_the_page_is_refused_rather_than_rounded_down() {
    assert_eq!(Plan::new(Some((0xc000_1000, PANEL)), Typing::Firmware), Err(Refusal::Unaligned(0xc000_1000)));
    // The refusal names the address, because a machine owner reads it.
    assert!(Refusal::Unaligned(0xc000_1000).to_string().contains("0xc0001000"));
}

#[test]
fn a_range_no_map_can_hold_is_refused_by_name() {
    // Past the two PDPTs' 512 GiB.
    assert_eq!(Plan::new(Some((512 * GIB, PANEL)), Typing::Firmware), Err(Refusal::PastPdpt(512)));
    // Its own end does not fit an address, either as it is given...
    let base = u64::MAX - PAGE_2M + 1;
    assert_eq!(Plan::new(Some((base, u64::MAX)), Typing::Firmware), Err(Refusal::Extent { base, len: u64::MAX }));
    // ...or once rounded up to the page it must end on.
    assert_eq!(Plan::new(Some((base, 1)), Typing::Firmware), Err(Refusal::Extent { base, len: 1 }));
    // Wider than the directories a plan may name, and told what it would need.
    assert_eq!(Plan::new(Some((16 * GIB, 10 * GIB)), Typing::Firmware), Err(Refusal::Directories(14)));
    assert!(Refusal::Directories(14).to_string().contains("14 page directories"));
}

/// The bits the x86-64 loader stores: PAT entry 3 is PCD and PWT with the PAT
/// bit clear, and entry 0 is none of the three.
#[test]
fn uncacheable_is_pat_entry_three_and_plain_memory_is_entry_zero() {
    const PRESENT_WRITABLE_2M: u64 = 1 | 1 << 1 | 1 << 7;
    const PWT: u64 = 1 << 3;
    const PCD: u64 = 1 << 4;
    const PAT_2M: u64 = 1 << 12;
    let at = 0x4020_0000;
    assert_eq!(x86_64::block(at, Cache::Firmware), at | PRESENT_WRITABLE_2M);
    assert_eq!(x86_64::block(at, Cache::Scanout), at | PRESENT_WRITABLE_2M | PCD | PWT);
    // The PAT bit is what would select entry 4, and nothing here sets it.
    for cache in [Cache::Firmware, Cache::Memory, Cache::Device, Cache::Scanout] {
        assert_eq!(x86_64::block(at, cache) & PAT_2M, 0, "{cache:?}");
    }
    assert_eq!(x86_64::table(0x1000), 0x1003);
}

/// QEMU `virt` with 1 GiB: flash and devices below 1 GiB, RAM from 1 GiB to 2
/// GiB, nothing above. What the map calls memory is memory, the rest a device's.
#[test]
fn a_map_typed_plan_is_memory_where_firmware_says_write_back_and_a_device_elsewhere() {
    let ram = [(0x4000_0000, 0x4000_0000)];
    let plan = Plan::new(None, Typing::ByMap(&ram)).expect("virt's map");
    for entry in plan.entries() {
        let want = if (0x4000_0000..0x8000_0000).contains(&entry.phys) { Cache::Memory } else { Cache::Device };
        assert_eq!(entry.cache, want, "{entry:?}");
    }
    is_consistent(&plan);
}

/// A page firmware says is write-back in part: no one type is right for it.
#[test]
fn a_page_that_is_part_memory_is_refused_by_address() {
    let ram = [(0x4000_0000, 0x4000_0000 + 0x1000)];
    assert_eq!(Plan::new(None, Typing::ByMap(&ram)), Err(Refusal::Mixed(0x8000_0000)));
    assert!(Refusal::Mixed(0x8000_0000).to_string().contains("0x80000000"));
    // Two adjacent ranges that meet inside a page make it whole.
    let split = [(0x4000_0000, 0x4010_0000 - 0x4000_0000), (0x4010_0000, 0x3ff0_0000)];
    assert!(Plan::new(None, Typing::ByMap(&split)).is_ok());
}

/// A scanout inside memory is the scanout's leaves, not a mixed page.
#[test]
fn a_scanout_inside_memory_is_typed_as_the_scanout() {
    let ram = [(0x4000_0000, 0x4000_0000)];
    let plan = Plan::new(Some((0x5000_0000, 0x10_0000)), Typing::ByMap(&ram)).expect("inside");
    let scanout: Vec<u64> =
        plan.entries().filter(|e| e.cache == Cache::Scanout).map(|e| e.phys).collect();
    assert_eq!(scanout, (0..256).map(|page| 0x5000_0000 + page * PAGE_4K).collect::<Vec<_>>());
    is_consistent(&plan);
    // And a scanout over a page that is part memory takes that page whole.
    let part = [(0x4000_0000, 0x1000)];
    assert!(Plan::new(Some((0x4000_0000, PAGE_2M)), Typing::ByMap(&part)).is_ok());
    assert_eq!(Plan::new(None, Typing::ByMap(&part)), Err(Refusal::Mixed(0x4000_0000)));
}

/// The AArch64 descriptors, against the Arm ARM's bit positions (K.a, D8.3.1
/// and D8.3.2): a table is 0b11; a block 0b01 with `AttrIndx` at 4:2, `SH` at
/// 9:8, `AF` at 10, `PXN` at 53 and `UXN` at 54; and `MAIR_EL1`'s bytes are the
/// Device-nGnRE, Normal write-back and Normal non-cacheable encodings of
/// D24.2.110.
#[test]
fn the_aarch64_descriptors_carry_the_attribute_index_the_mair_names() {
    let at = 0x4020_0000;
    let attr_index = |d: u64| (d >> 2) & 0b111;
    let memory = aarch64::block(at, Cache::Memory);
    let device = aarch64::block(at, Cache::Device);
    let scanout = aarch64::block(at, Cache::Scanout);
    for d in [memory, device, scanout] {
        assert_eq!(d & 0b11, 0b01, "a block");
        assert_ne!(d & 1 << 10, 0, "the access flag");
        assert_eq!(d & 0b11 << 6, 0, "EL1 read-write, EL0 nothing");
        assert_ne!(d & 1 << 54, 0, "never executable at EL0");
        assert_eq!(d & 0x0000_FFFF_FFE0_0000, at, "the output address");
    }
    assert_eq!(aarch64::ATTRS[attr_index(memory) as usize], 0xFF);
    assert_eq!(aarch64::ATTRS[attr_index(device) as usize], 0x04);
    assert_eq!(aarch64::ATTRS[attr_index(scanout) as usize], 0x44);
    assert_eq!(memory & 1 << 53, 0, "the kernel executes from memory");
    assert_ne!(device & 1 << 53, 0, "and never from a device");
    assert_eq!((memory >> 8) & 0b11, 0b11, "memory is inner shareable");
    assert_eq!(aarch64::MAIR, 0x44_FF_04);
    // A level-3 page is 0b11 with the same attributes.
    assert_eq!(aarch64::page(0x5000_1000, Cache::Scanout), (scanout & !0x0000_FFFF_FFFF_F000 & !0b11) | 0x5000_1000 | 0b11);
    assert_eq!(aarch64::table(0x1000), 0x1003);
}

/// QEMU `virt`'s ramfb as AAVMF placed it on a real boot: 3 MiB carved out of
/// RAM at 0xbc7a0000, on no 2 MiB boundary. Its first and last 2 MiB pages
/// are split, so the scanout's type reaches exactly its own bytes and the RAM
/// beside it stays memory; the page wholly inside it stays one leaf.
#[test]
fn a_scanout_carved_out_of_ram_splits_the_pages_it_shares() {
    let ram = [(0x4000_0000, 0x8000_0000)];
    let (base, len) = (0xbc7a_0000, 0x30_0000);
    let plan = Plan::new(Some((base, len)), Typing::ByMap(&ram)).expect("ramfb");
    assert_eq!(plan.fine_tables(), [0xbc60_0000, 0xbca0_0000]);
    let scanout: Vec<Entry> = plan.entries().filter(|e| e.cache == Cache::Scanout).collect();
    let bytes: u64 = scanout
        .iter()
        .map(|e| match e.slot {
            Slot::Directory { .. } => PAGE_2M,
            Slot::Fine { .. } => PAGE_4K,
        })
        .sum();
    assert_eq!(bytes, len, "the scanout's type covers its own bytes and none beside them");
    assert!(scanout.iter().all(|e| e.phys >= base && e.phys < base + len));
    // The 4 KiB leaf just below it and just past it are memory.
    let at = |phys: u64| plan.entries().find(|e| e.phys == phys).expect("mapped").cache;
    assert_eq!(at(base - PAGE_4K), Cache::Memory);
    assert_eq!(at(base + len), Cache::Memory);
    assert_eq!(at(0xbc80_0000), Cache::Scanout);
    is_consistent(&plan);
    // The same framebuffer under range-register typing is refused, as before.
    assert_eq!(Plan::new(Some((base, len)), Typing::Firmware), Err(Refusal::Unaligned(base)));
}

/// A map-typed scanout must still start on a 4 KiB page.
#[test]
fn a_map_typed_scanout_off_a_small_page_is_refused() {
    let ram = [(0x4000_0000, 0x8000_0000)];
    assert_eq!(
        Plan::new(Some((0xbc7a_0800, 0x1000)), Typing::ByMap(&ram)),
        Err(Refusal::Unaligned(0xbc7a_0800))
    );
}
