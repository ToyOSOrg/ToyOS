//! Host page tables (§2.2.3): 4 KiB tables of 512 eight-byte entries, a
//! four-level walk over a 48-bit device address (Table 15), ending at level 2
//! in a translation entry for a 2 MiB page (Figures 9 and 10, Tables 17 and
//! 18). No level is skipped.

use toyos_phys::Phys;

const PR: u64 = 1 << 0;
const NEXT_LEVEL_SHIFT: u32 = 9;
const NEXT_LEVEL: u64 = 0b111 << NEXT_LEVEL_SHIFT;
/// Force Coherent: the unit marks the request snooped whatever the device
/// asked, so a device's no-snoop setting cannot read stale memory.
const FC: u64 = 1 << 60;
const IR: u64 = 1 << 61;
const IW: u64 = 1 << 62;

/// The level the root table is at, `Mode` of the entry naming it.
pub const ROOT_LEVEL: u8 = crate::dte::MODE;
/// The level whose translation entries are 2 MiB pages.
pub const LEAF_LEVEL: u8 = 2;

/// The index `at` takes in the table at `level` (Table 15: bits
/// `12 + 9 * (level - 1)` up).
pub const fn index(at: u64, level: u8) -> usize {
    ((at >> (12 + 9 * (level as u32 - 1))) & 0x1FF) as usize
}

/// An entry at `level` pointing at the table one level down, granting both
/// read and write, which the walk ANDs down the levels (Table 18).
pub const fn directory(below: Phys<12>, level: u8) -> u64 {
    PR | ((level as u64 - 1) << NEXT_LEVEL_SHIFT) | below.get() | IR | IW
}

/// A level-2 translation entry for the 2 MiB page at `page`, read and write
/// (Table 17, `NextLevel` 000b).
pub const fn leaf(page: Phys<21>) -> u64 {
    PR | page.get() | FC | IR | IW
}

/// What one entry is, by `PR` and `NextLevel`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    Absent,
    Directory(Phys<12>),
    Leaf,
}

pub const fn entry(word: u64) -> Entry {
    if word & PR == 0 {
        return Entry::Absent;
    }
    match (word & NEXT_LEVEL) >> NEXT_LEVEL_SHIFT {
        0 | 0b111 => Entry::Leaf,
        _ => Entry::Directory(Phys::of(word)),
    }
}
