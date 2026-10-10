//! The machines this decision is made for: a framebuffer inside the low map,
//! one above it, one on the boundary between them, and one that fits no map;
//! and a loader firmware put below the map's end or above it.

use toyos_bootmap::{
    aarch64, x86_64, Cache, Entry, Plan, Refusal, Slot, Typing, BOOT_MAP_BYTES, DIRECT_MAP_WINDOW, MAX_PAGES,
    PAGE_2M, PAGE_4K, ROOT_HIGH_HALF, ROOT_IDENTITY,
};

const GIB: u64 = 1 << 30;
/// 1920x1080x4, which is neither a whole page nor a whole GiB.
const PANEL: u64 = 0x7e9000;
/// The loader as OVMF loads it on a 2 GiB guest: relocated low, and not on a page.
const LOADER: (u64, u64) = (0x7e5f_0000, 0x3_5000);
/// A CPU of 48 physical address bits, wider than any address these tests name
/// but the ones that ask for the width itself.
const BITS: u32 = 48;

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
    // Every directory is named from the second-level table over its own GiB,
    // at its own index, and no two from one place.
    let slots: Vec<(usize, usize)> = plan.directory_slots().collect();
    for (&gib, &(region, index)) in plan.directories().iter().zip(&slots) {
        assert_eq!(plan.regions()[region] * 512 + index as u64, gib, "directory {gib}");
    }
    for (at, slot) in slots.iter().enumerate() {
        assert!(!slots[..at].contains(slot), "{slot:?} twice");
    }
    // Every table is named from an identity slot below the high half and a
    // high-half slot inside the root.
    for &region in plan.regions() {
        assert!(ROOT_IDENTITY + (region as usize) < ROOT_HIGH_HALF, "region {region}");
        assert!(ROOT_HIGH_HALF + (region as usize) < 512, "region {region}");
    }
    assert!(1 + plan.regions().len() + plan.directories().len() + plan.fine_tables().len() <= MAX_PAGES);
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
    let plan = Plan::new(None, LOADER, Typing::Firmware, BITS).expect("the low map alone");
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
    let plan = Plan::new(Some((0xc000_0000, PANEL)), LOADER, Typing::Firmware, BITS).expect("inside the low map");
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
    let plan = Plan::new(Some((256 * GIB, PANEL)), LOADER, Typing::Firmware, BITS).expect("above the low map");
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
    let plan = Plan::new(Some((BOOT_MAP_BYTES, PANEL)), LOADER, Typing::Firmware, BITS).expect("at the boundary");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 4]);
    assert!(plan.entries().filter(|e| e.cache == Cache::Scanout).all(|e| directory(&e) == 4));
    is_consistent(&plan);

    // One page below it is the low map's last page, and adds nothing.
    let inside = Plan::new(Some((BOOT_MAP_BYTES - PAGE_2M, PAGE_2M)), LOADER, Typing::Firmware, BITS).expect("the last page");
    assert_eq!(inside.directories(), [0, 1, 2, 3]);
    is_consistent(&inside);
}

/// A range that straddles the low map's own end: its first page retypes an
/// entry the low map already holds and the rest are emitted beside it, so both
/// arms of `entries` contribute to one scanout.
#[test]
fn a_framebuffer_that_straddles_the_low_maps_end_is_mapped_from_both_arms() {
    let base = BOOT_MAP_BYTES - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), LOADER, Typing::Firmware, BITS).expect("across the end");
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
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), LOADER, Typing::Firmware, BITS).expect("straddling");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 7, 8]);
    assert!(plan.fine_tables().is_empty());
    is_consistent(&plan);

    // With a loader straddling another, every directory the budget has; the
    // two split-page tables are a map-typed plan's, and this one splits nothing.
    let loader = (12 * GIB - 0x1000, 0x3_5000);
    let full = Plan::new(Some((base, 4 * PAGE_2M)), loader, Typing::Firmware, BITS).expect("both straddling");
    assert_eq!(full.directories(), [0, 1, 2, 3, 7, 8, 11, 12]);
    assert_eq!(full.regions(), [0]);
    is_consistent(&full);
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
    assert_eq!(Plan::new(Some((0xc000_1000, PANEL)), LOADER, Typing::Firmware, BITS), Err(Refusal::Unaligned(0xc000_1000)));
    // The refusal names the address, because a machine owner reads it.
    assert!(Refusal::Unaligned(0xc000_1000).to_string().contains("0xc0001000"));
}

#[test]
fn a_range_no_map_can_hold_is_refused_by_name() {
    // Its own end does not fit an address, either as it is given...
    let base = u64::MAX - PAGE_2M + 1;
    assert_eq!(Plan::new(Some((base, u64::MAX)), LOADER, Typing::Firmware, BITS), Err(Refusal::Extent { base, len: u64::MAX }));
    // ...or once rounded up to the page it must end on.
    assert_eq!(Plan::new(Some((base, 1)), LOADER, Typing::Firmware, BITS), Err(Refusal::Extent { base, len: 1 }));
    // Wider than the directories a plan may name, and told what it would need.
    assert_eq!(Plan::new(Some((16 * GIB, 10 * GIB)), LOADER, Typing::Firmware, BITS), Err(Refusal::Directories(14)));
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
    let plan = Plan::new(None, LOADER, Typing::ByMap(&ram), BITS).expect("virt's map");
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
    assert_eq!(Plan::new(None, LOADER, Typing::ByMap(&ram), BITS), Err(Refusal::Mixed(0x8000_0000)));
    assert!(Refusal::Mixed(0x8000_0000).to_string().contains("0x80000000"));
    // Two adjacent ranges that meet inside a page make it whole.
    let split = [(0x4000_0000, 0x4010_0000 - 0x4000_0000), (0x4010_0000, 0x3ff0_0000)];
    assert!(Plan::new(None, LOADER, Typing::ByMap(&split), BITS).is_ok());
}

/// A scanout inside memory is the scanout's leaves, not a mixed page.
#[test]
fn a_scanout_inside_memory_is_typed_as_the_scanout() {
    let ram = [(0x4000_0000, 0x4000_0000)];
    let plan = Plan::new(Some((0x5000_0000, 0x10_0000)), LOADER, Typing::ByMap(&ram), BITS).expect("inside");
    let scanout: Vec<u64> =
        plan.entries().filter(|e| e.cache == Cache::Scanout).map(|e| e.phys).collect();
    assert_eq!(scanout, (0..256).map(|page| 0x5000_0000 + page * PAGE_4K).collect::<Vec<_>>());
    is_consistent(&plan);
    // And a scanout over a page that is part memory takes that page whole.
    let part = [(0x4000_0000, 0x1000)];
    assert!(Plan::new(Some((0x4000_0000, PAGE_2M)), LOADER, Typing::ByMap(&part), BITS).is_ok());
    assert_eq!(Plan::new(None, LOADER, Typing::ByMap(&part), BITS), Err(Refusal::Mixed(0x4000_0000)));
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
    let plan = Plan::new(Some((base, len)), LOADER, Typing::ByMap(&ram), BITS).expect("ramfb");
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
    assert_eq!(Plan::new(Some((base, len)), LOADER, Typing::Firmware, BITS), Err(Refusal::Unaligned(base)));
}

/// A map-typed scanout must still start on a 4 KiB page.
#[test]
fn a_map_typed_scanout_off_a_small_page_is_refused() {
    let ram = [(0x4000_0000, 0x8000_0000)];
    assert_eq!(
        Plan::new(Some((0xbc7a_0800, 0x1000)), LOADER, Typing::ByMap(&ram), BITS),
        Err(Refusal::Unaligned(0xbc7a_0800))
    );
}

/// Where lld-link's default `ImageBase` puts the loader when firmware can honour
/// it, 5 GiB on a 4 GiB q35 guest: past the low map, so the `mov cr3` would
/// fetch its next instruction from nowhere unless the map adds it.
#[test]
fn a_loader_above_the_low_map_adds_its_own_directory_of_plain_memory() {
    let loader = (0x1_4000_0000, 0x3_4a00);
    let plan = Plan::new(None, loader, Typing::Firmware, BITS).expect("above the low map");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 5]);
    assert_eq!(plan.loader(), (0x1_4000_0000, PAGE_2M));
    let mine: Vec<(u64, usize, Cache)> = plan
        .entries()
        .filter(|e| e.phys >= BOOT_MAP_BYTES)
        .map(|e| (e.phys, directory(&e), e.cache))
        .collect();
    assert_eq!(mine, [(0x1_4000_0000, 4, Cache::Firmware)]);
    is_consistent(&plan);
}

/// A loader inside the low map is already mapped, and adds nothing.
#[test]
fn a_loader_inside_the_low_map_adds_nothing() {
    let plan = Plan::new(None, LOADER, Typing::Firmware, BITS).expect("inside");
    assert_eq!(plan.directories(), [0, 1, 2, 3]);
    assert_eq!(plan.entries().count() as u64, BOOT_MAP_BYTES / PAGE_2M);
    assert_eq!(plan.loader(), (0x7e40_0000, 2 * PAGE_2M));
}

/// A loader off the page is rounded out both ways, and one that crosses a page
/// is both pages: its code may run from either.
#[test]
fn a_loader_across_a_page_is_both_pages() {
    let loader = (6 * GIB + PAGE_2M - 0x1000, 0x3_4a00);
    let plan = Plan::new(None, loader, Typing::Firmware, BITS).expect("across a page");
    assert_eq!(plan.loader(), (6 * GIB, 2 * PAGE_2M));
    let mine: Vec<u64> = plan.entries().filter(|e| e.phys >= BOOT_MAP_BYTES).map(|e| e.phys).collect();
    assert_eq!(mine, [6 * GIB, 6 * GIB + PAGE_2M]);
    is_consistent(&plan);
}

/// A loader sharing a GiB with the scanout shares its directory, and a page the
/// scanout holds is not written twice.
#[test]
fn a_loader_beside_the_scanout_shares_its_directory() {
    let plan = Plan::new(Some((256 * GIB, PANEL)), (256 * GIB + 0x7f_0000, 0x3_4a00), Typing::Firmware, BITS)
        .expect("one GiB for both");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 256]);
    let above: Vec<(u64, Cache)> =
        plan.entries().filter(|e| e.phys >= BOOT_MAP_BYTES).map(|e| (e.phys, e.cache)).collect();
    assert_eq!(
        above,
        [
            (256 * GIB, Cache::Scanout),
            (256 * GIB + PAGE_2M, Cache::Scanout),
            (256 * GIB + 2 * PAGE_2M, Cache::Scanout),
            (256 * GIB + 3 * PAGE_2M, Cache::Scanout),
            (256 * GIB + 4 * PAGE_2M, Cache::Firmware),
        ]
    );
    is_consistent(&plan);
}

/// Under a map's typing the loader's pages are memory, and a page of it that
/// firmware says is memory only in part is refused like any other.
#[test]
fn a_map_typed_loader_is_memory() {
    let ram = [(0x4000_0000, 0x4000_0000), (5 * GIB, GIB)];
    let plan = Plan::new(None, (5 * GIB + 0x1000, 0x3_4a00), Typing::ByMap(&ram), BITS).expect("in memory");
    let above: Vec<(u64, Cache)> =
        plan.entries().filter(|e| e.phys >= BOOT_MAP_BYTES).map(|e| (e.phys, e.cache)).collect();
    assert_eq!(above, [(5 * GIB, Cache::Memory)]);
    is_consistent(&plan);
    let part = [(0x4000_0000, 0x4000_0000), (5 * GIB, 0x10_0000)];
    assert_eq!(
        Plan::new(None, (5 * GIB, 0x3_4a00), Typing::ByMap(&part), BITS),
        Err(Refusal::Mixed(5 * GIB))
    );
}

#[test]
fn a_loader_no_map_can_hold_is_refused_by_name() {
    assert_eq!(
        Plan::new(None, (0x1_4000_0000, 0), Typing::Firmware, BITS),
        Err(Refusal::Extent { base: 0x1_4000_0000, len: 0 })
    );
    // Its directories count against the same budget as the scanout's, and a
    // GiB both need is counted once.
    assert_eq!(
        Plan::new(Some((16 * GIB, 4 * GIB)), (32 * GIB, 0x3_4a00), Typing::Firmware, BITS),
        Err(Refusal::Directories(9))
    );
    let shared = Plan::new(Some((16 * GIB, 4 * GIB)), (17 * GIB, 0x3_4a00), Typing::Firmware, BITS)
        .expect("the loader inside the scanout's GiBs");
    assert_eq!(shared.directories(), [0, 1, 2, 3, 16, 17, 18, 19]);
    is_consistent(&shared);
}

/// A framebuffer firmware put at 1010 GiB, as an AMD laptop's does, and the
/// panel its GOP reports there: past the low map's 512 GiB, so the map adds a
/// second-level table for it, named from both views' second root slot.
#[test]
fn a_framebuffer_past_512_gib_adds_its_own_second_level_table() {
    let base = 0xfc_9000_0000;
    let plan = Plan::new(Some((base, 8_294_400)), LOADER, Typing::Firmware, BITS).expect("at 1010 GiB");
    assert_eq!(base / GIB, 1010);
    assert_eq!(plan.directories(), [0, 1, 2, 3, 1010]);
    assert_eq!(plan.regions(), [0, 1]);
    assert_eq!(plan.directory_slots().last(), Some((1, 1010 - 512)));
    let mine: Vec<(u64, usize)> =
        plan.entries().filter(|e| e.cache == Cache::Scanout).map(|e| (e.phys, directory(&e))).collect();
    assert_eq!(mine, (0..4).map(|page| (base + page * PAGE_2M, 4)).collect::<Vec<_>>());
    is_consistent(&plan);
}

/// A scanout across the first 512 GiB boundary is two directories under two
/// second-level tables, and a loader in the same 512 GiB shares the second.
#[test]
fn a_framebuffer_across_512_gib_names_both_second_level_tables() {
    let base = 512 * GIB - PAGE_2M;
    let plan = Plan::new(Some((base, 4 * PAGE_2M)), (520 * GIB, 0x3_4a00), Typing::Firmware, BITS)
        .expect("across 512 GiB");
    assert_eq!(plan.directories(), [0, 1, 2, 3, 511, 512, 520]);
    assert_eq!(plan.regions(), [0, 1]);
    assert_eq!(plan.directory_slots().skip(4).collect::<Vec<_>>(), [(0, 511), (1, 0), (1, 8)]);
    is_consistent(&plan);
}

/// The most the map can be asked for: a scanout and a loader each straddling a
/// 512 GiB boundary, four second-level tables past the low map's, the last
/// below the high half's last root slot.
#[test]
fn every_second_level_table_the_budget_has_fits_the_pool() {
    let scanout = (1024 * GIB - PAGE_2M, 4 * PAGE_2M);
    let loader = (DIRECT_MAP_WINDOW - 512 * GIB - 0x1000, 0x3_4a00);
    let plan = Plan::new(Some(scanout), loader, Typing::Firmware, BITS).expect("both straddling");
    assert_eq!(plan.regions(), [0, 1, 2, 254, 255]);
    assert_eq!(1 + plan.regions().len() + plan.directories().len() + plan.fine_tables().len(), MAX_PAGES - 2);
    is_consistent(&plan);
}

/// The CPU's width is the map's reach: a range ending on its last byte fits,
/// and one a page past it is refused by name, for the scanout and the loader.
#[test]
fn a_range_past_the_physical_address_width_is_refused_by_name() {
    let top = 1 << 40;
    let last = Plan::new(Some((top - 4 * PAGE_2M, 4 * PAGE_2M)), LOADER, Typing::Firmware, 40).expect("the last bytes");
    assert_eq!(last.directories(), [0, 1, 2, 3, 1023]);
    assert_eq!(last.regions(), [0, 1]);
    is_consistent(&last);
    assert_eq!(
        Plan::new(Some((top - 2 * PAGE_2M, 4 * PAGE_2M)), LOADER, Typing::Firmware, 40),
        Err(Refusal::PastWidth { end: top + 2 * PAGE_2M, bits: 40 })
    );
    assert_eq!(
        Plan::new(None, (top - 0x1000, 0x3_4a00), Typing::Firmware, 40),
        Err(Refusal::PastWidth { end: top + PAGE_2M, bits: 40 })
    );
    let said = Refusal::PastWidth { end: top + PAGE_2M, bits: 40 }.to_string();
    assert!(said.contains("0x10000200000") && said.contains("40 bits"), "{said}");
}

/// Past the high half's 128 TiB no view can hold a range, whatever the CPU's
/// width: the identity view would reach into the high half's slots.
#[test]
fn a_range_past_the_window_is_refused_however_wide_the_cpu() {
    assert_eq!(
        Plan::new(Some((DIRECT_MAP_WINDOW, PANEL)), LOADER, Typing::Firmware, 52),
        Err(Refusal::PastWindow(DIRECT_MAP_WINDOW + 0x80_0000))
    );
    assert_eq!(
        Plan::new(None, (DIRECT_MAP_WINDOW - 0x1000, 0x3_4a00), Typing::Firmware, 52),
        Err(Refusal::PastWindow(DIRECT_MAP_WINDOW + PAGE_2M))
    );
    let last = Plan::new(Some((DIRECT_MAP_WINDOW - 0x80_0000, PANEL)), LOADER, Typing::Firmware, 52).expect("the window's last GiB");
    assert_eq!(last.regions(), [0, 255]);
    is_consistent(&last);
}

/// `PARange`'s encodings as the Arm ARM gives them, and a reserved one refused.
#[test]
fn parange_decodes_to_the_widths_the_arm_arm_names() {
    let widths: Vec<Option<u32>> = (0..9).map(aarch64::physical_bits).collect();
    assert_eq!(widths, [Some(32), Some(36), Some(40), Some(42), Some(44), Some(48), Some(52), Some(56), None]);
}
