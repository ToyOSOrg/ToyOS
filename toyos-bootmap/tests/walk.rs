//! The tables `Plan::write` lays down, walked as an x86-64 CPU walks them: Intel
//! SDM Vol. 3A §4.5, four-level paging, read from the entries' bits here rather
//! than from the encoder, so a table named from the wrong slot or the wrong
//! second-level table is an address that does not translate.

use toyos_bootmap::{x86_64, Cache, Plan, Table, Typing, BOOT_MAP_BYTES, MAX_PAGES, PAGE_2M, PAGE_4K};

const GIB: u64 = 1 << 30;
const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;
/// Where the pool sits in physical memory: inside the low map, as the loader's
/// allocation does.
const AT: u64 = 0x7f80_0000;
/// The loader as OVMF loads it on a 2 GiB guest.
const LOADER: (u64, u64) = (0x7e5f_0000, 0x3_5000);
/// The AMD laptop's framebuffer, as its GOP reports it.
const LAPTOP: (u64, u64) = (0xfc_9000_0000, 8_294_400);

/// Bits M-1:12 of an entry that names a page, for the widest M (52) §4.1.4 allows.
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
const P: u64 = 1 << 0;
const RW: u64 = 1 << 1;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const PS: u64 = 1 << 7;

/// What a linear address translates to: the physical address, the PAT entry
/// its type comes from (§11.12.3: PAT·4 + PCD·2 + PWT), and whether a
/// supervisor write is allowed (R/W set at every level, §4.6.1).
#[derive(Debug, PartialEq, Eq)]
struct Translation {
    phys: u64,
    pat: u64,
    writable: bool,
}

/// The pool as physical memory: the 8 bytes at `phys`, which must lie in it.
fn read(pool: &[Table; MAX_PAGES], phys: u64) -> u64 {
    assert!((AT..AT + (MAX_PAGES as u64) * PAGE_4K).contains(&phys), "an entry names {phys:#x}, outside the pool");
    let offset = phys - AT;
    pool[(offset / PAGE_4K) as usize].0[((offset % PAGE_4K) / 8) as usize]
}

/// SDM Vol. 3A §4.5.4, Figures 4-8 to 4-10: CR3, then a PML4E by bits 47:39, a
/// PDPTE by 38:30, a PDE by 29:21 and a PTE by 20:12, each found at the
/// address bits of the one above; a PS bit ends the walk at a 1 GiB or 2 MiB
/// page. `None` is a not-present entry at some level.
fn walk(pool: &[Table; MAX_PAGES], cr3: u64, linear: u64) -> Option<Translation> {
    // §3.3.7.1: bits 63:48 copy bit 47.
    let high = linear >> 47;
    assert!(high == 0 || high == 0x1_FFFF, "{linear:#x} is not canonical");
    let index = |shift: u32| (linear >> shift) & 0x1FF;
    let pml4e = read(pool, (cr3 & ADDRESS) | index(39) << 3);
    if pml4e & P == 0 {
        return None;
    }
    let pdpte = read(pool, (pml4e & ADDRESS) | index(30) << 3);
    if pdpte & P == 0 {
        return None;
    }
    let mut writable = pml4e & pdpte & RW != 0;
    if pdpte & PS != 0 {
        // Table 4-16: a 1 GiB page, bits 51:30, its PAT bit 12.
        let phys = (pdpte & ADDRESS & !(GIB - 1)) | (linear & (GIB - 1));
        return Some(Translation { phys, pat: pat(pdpte, pdpte >> 12 & 1), writable });
    }
    let pde = read(pool, (pdpte & ADDRESS) | index(21) << 3);
    if pde & P == 0 {
        return None;
    }
    writable &= pde & RW != 0;
    if pde & PS != 0 {
        // Table 4-17: a 2 MiB page, bits 51:21; bits 20:13 are reserved.
        assert_eq!(pde & 0x1F_E000, 0, "PDE {pde:#x} sets a reserved bit");
        let phys = (pde & ADDRESS & !(PAGE_2M - 1)) | (linear & (PAGE_2M - 1));
        return Some(Translation { phys, pat: pat(pde, pde >> 12 & 1), writable });
    }
    let pte = read(pool, (pde & ADDRESS) | index(12) << 3);
    if pte & P == 0 {
        return None;
    }
    // Table 4-20: a 4 KiB page, bits 51:12, its PAT bit 7.
    let phys = (pte & ADDRESS) | (linear & (PAGE_4K - 1));
    Some(Translation { phys, pat: pat(pte, pte >> 7 & 1), writable: writable && pte & RW != 0 })
}

fn pat(entry: u64, pat_bit: u64) -> u64 {
    pat_bit << 2 | u64::from(entry & PCD != 0) << 1 | u64::from(entry & PWT != 0)
}

/// `plan` written at [`AT`] into a pool whose every entry is a stale present
/// one naming memory outside it, and its root.
fn written(plan: &Plan) -> (Box<[Table; MAX_PAGES]>, u64) {
    let mut pool: Box<[Table; MAX_PAGES]> =
        vec![Table([u64::MAX; 512]); MAX_PAGES].into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!());
    let root = plan.write(x86_64::ENCODING, &mut pool, AT);
    (pool, root)
}

/// The PAT entry a leaf of this kind must select: entry 0, whose type is the
/// MTRRs', for memory; entry 3, uncacheable under every MTRR and under the
/// power-on PAT, for a device and the scanout.
fn expected_pat(cache: Cache) -> u64 {
    match cache {
        Cache::Firmware | Cache::Memory => 0,
        Cache::Device | Cache::Scanout => 3,
    }
}

/// Every leaf the plan names translates, at identity and at `PHYS_OFFSET`, its
/// first byte and its last, to itself, writable and of its kind's type.
fn every_leaf_translates_in_both_views(plan: &Plan, pool: &[Table; MAX_PAGES], root: u64) {
    for entry in plan.entries() {
        let size = match entry.slot {
            toyos_bootmap::Slot::Directory { .. } => PAGE_2M,
            toyos_bootmap::Slot::Fine { .. } => PAGE_4K,
        };
        for phys in [entry.phys, entry.phys + size - 1] {
            let want = Translation { phys, pat: expected_pat(entry.cache), writable: true };
            assert_eq!(walk(pool, root, phys).as_ref(), Some(&want), "{entry:?} at identity");
            assert_eq!(walk(pool, root, PHYS_OFFSET + phys).as_ref(), Some(&want), "{entry:?} at PHYS_OFFSET");
        }
    }
}

/// The laptop that stopped in the loader: its scanout at 1010 GiB, under the
/// second 512 GiB's table, reached from both views, while the high half's
/// first slot still reaches GiB 0.
#[test]
fn the_laptops_scanout_at_1010_gib_translates_in_both_views() {
    let (base, len) = LAPTOP;
    let plan = Plan::new(Some(LAPTOP), LOADER, Typing::Firmware).expect("the laptop's plan");
    let (pool, root) = written(&plan);
    assert_eq!(root, AT, "the root is the pool's first page");

    let scanout = |phys: u64| Some(Translation { phys, pat: 3, writable: true });
    let memory = |phys: u64| Some(Translation { phys, pat: 0, writable: true });
    for phys in [base, base + len - 1] {
        assert_eq!(walk(&pool, root, phys), scanout(phys), "identity {phys:#x}");
        assert_eq!(walk(&pool, root, PHYS_OFFSET + phys), scanout(phys), "PHYS_OFFSET + {phys:#x}");
    }
    for phys in [0, BOOT_MAP_BYTES - 1, LOADER.0] {
        assert_eq!(walk(&pool, root, phys), memory(phys), "identity {phys:#x}");
        assert_eq!(walk(&pool, root, PHYS_OFFSET + phys), memory(phys), "PHYS_OFFSET + {phys:#x}");
    }
    // Nothing but what the plan names: past the low map, past the scanout's
    // last page in its GiB, and the 512 GiB below it.
    let past_scanout = base + len.next_multiple_of(PAGE_2M);
    for phys in [BOOT_MAP_BYTES, past_scanout, 512 * GIB, base - PAGE_2M] {
        assert_eq!(walk(&pool, root, phys), None, "identity {phys:#x}");
        assert_eq!(walk(&pool, root, PHYS_OFFSET + phys), None, "PHYS_OFFSET + {phys:#x}");
    }
    every_leaf_translates_in_both_views(&plan, &pool, root);
}

/// A scanout across 512 GiB with the loader past it, and the whole budget of
/// second-level tables: each directory under its own 512 GiB's table.
#[test]
fn every_second_level_table_is_reached_from_its_own_root_slots() {
    let across = Plan::new(Some((512 * GIB - PAGE_2M, 4 * PAGE_2M)), (520 * GIB, 0x3_4a00), Typing::Firmware)
        .expect("across 512 GiB");
    let (pool, root) = written(&across);
    every_leaf_translates_in_both_views(&across, &pool, root);

    let budget = Plan::new(
        Some((1024 * GIB - PAGE_2M, 4 * PAGE_2M)),
        (toyos_bootmap::DIRECT_MAP_WINDOW - 512 * GIB - 0x1000, 0x3_4a00),
        Typing::Firmware,
    )
    .expect("both straddling");
    assert_eq!(budget.regions(), [0, 1, 2, 254, 255]);
    let (pool, root) = written(&budget);
    every_leaf_translates_in_both_views(&budget, &pool, root);
}

/// A scanout carved out of RAM, whose shared pages are split: the 4 KiB tables
/// hang under the right directory, and the bytes beside the scanout stay memory.
#[test]
fn a_split_page_translates_through_its_table_of_small_leaves() {
    let ram = [(0x4000_0000, 0x8000_0000)];
    let (base, len) = (0xbc7a_0000, 0x30_0000);
    let plan = Plan::new(Some((base, len)), LOADER, Typing::ByMap(&ram)).expect("ramfb");
    let (pool, root) = written(&plan);
    assert_eq!(walk(&pool, root, base - 1), Some(Translation { phys: base - 1, pat: 0, writable: true }));
    assert_eq!(walk(&pool, root, base), Some(Translation { phys: base, pat: 3, writable: true }));
    every_leaf_translates_in_both_views(&plan, &pool, root);
}
