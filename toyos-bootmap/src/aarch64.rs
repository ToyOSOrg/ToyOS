//! AArch64's encoding of a [`Plan`](crate::Plan): the VMSAv8-64 stage 1
//! descriptors of a 4 KiB granule (Arm ARM K.a, D8.3), and the one `MAIR_EL1`
//! whose indices they name — the loader writes the one, the kernel's entry
//! loads the other, and both read them here. And which pages the kernel's own
//! direct map holds, since on AArch64 that is decided page by page.

use toyos_abi::boot::MemoryMapEntry;

use crate::{is_read_as_memory, Cache, DirectMapEnd, Refusal, DIRECT_MAP_WINDOW, PAGE_2M, PAGE_4K};

/// `MAIR_EL1` index 0: Device-nGnRE, registers.
pub const ATTR_DEVICE: u64 = 0;
/// `MAIR_EL1` index 1: Normal, inner and outer write-back, read- and
/// write-allocate — RAM.
pub const ATTR_NORMAL: u64 = 1;
/// `MAIR_EL1` index 2: Normal, inner and outer non-cacheable — the scanout,
/// whose stores gather as write-combining ones do.
pub const ATTR_NORMAL_NC: u64 = 2;
/// Each index's attribute byte (D24.2.110), in index order: the value
/// `PAR_EL1.ATTR` also reports a translation's type in.
pub const ATTRS: [u8; 3] = [0x04, 0xFF, 0x44];
/// `MAIR_EL1` whole.
pub const MAIR: u64 = ATTRS[0] as u64 | (ATTRS[1] as u64) << 8 | (ATTRS[2] as u64) << 16;

/// A table descriptor (bits 1:0 = 0b11) naming the next table down.
const TABLE: u64 = 0b11;
/// A block descriptor (bits 1:0 = 0b01) at level 1 or 2.
const BLOCK: u64 = 0b01;
/// `SH` = inner shareable, bits 9:8.
const INNER_SHAREABLE: u64 = 0b11 << 8;
/// `SH` = outer shareable.
const OUTER_SHAREABLE: u64 = 0b10 << 8;
/// The access flag, bit 10: set, so the first access does not fault.
const AF: u64 = 1 << 10;
/// Privileged execute-never, bit 53.
pub const PXN: u64 = 1 << 53;
/// Unprivileged execute-never, bit 54.
const UXN: u64 = 1 << 54;

/// A page descriptor (bits 1:0 = 0b11) at level 3.
const PAGE: u64 = 0b11;

/// A descriptor naming the next table down, at `phys`.
pub const fn table(phys: u64) -> u64 {
    phys | TABLE
}

/// A 2 MiB block at `phys`: EL1 read-write and EL0 nothing (`AP` = 0b00), and
/// executable at EL1 only where it is memory — the kernel's image is. A
/// device is never executable, since a speculative fetch from one is a read
/// of its registers.
pub const fn block(phys: u64, cache: Cache) -> u64 {
    phys | BLOCK | AF | attributes(cache)
}

const fn attributes(cache: Cache) -> u64 {
    match cache {
        Cache::Memory => ATTR_NORMAL << 2 | INNER_SHAREABLE | UXN,
        Cache::Device => ATTR_DEVICE << 2 | PXN | UXN,
        Cache::Scanout => ATTR_NORMAL_NC << 2 | OUTER_SHAREABLE | PXN | UXN,
        Cache::Firmware => panic!("AArch64 has no range registers to type memory: its plan types by the map"),
    }
}

/// A 4 KiB page at `phys`, with [`block`]'s attributes.
pub const fn page(phys: u64, cache: Cache) -> u64 {
    phys | PAGE | AF | attributes(cache)
}

/// 4 KiB pages in one 2 MiB page.
const PAGES: u64 = PAGE_2M / PAGE_4K;

/// How the kernel's own direct map holds one 2 MiB page of physical memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coverage {
    /// No byte of it is memory the kernel reads: it is not mapped.
    Nothing,
    /// Every byte is: one block.
    Whole,
    /// Some of its 4 KiB pages are: a table whose page `i` is mapped where bit
    /// `i % 64` of word `i / 64` is set.
    Pages([u64; 8]),
}

impl Coverage {
    /// Whether the 4 KiB page `index` of the 2 MiB page is mapped.
    pub const fn holds(&self, index: u64) -> bool {
        match self {
            Self::Nothing => false,
            Self::Whole => true,
            Self::Pages(bits) => bits[(index / 64) as usize] & 1 << (index % 64) != 0,
        }
    }
}

/// One past the kernel direct map's last byte on AArch64: the end of the
/// highest range the kernel reads as memory, in whole 2 MiB pages. Nothing
/// below it is mapped for being low, as x86-64's low pages are: a page is
/// mapped only for being memory ([`coverage`]), because a register or a hole
/// mapped Normal is one a speculative access can reach. A range off a 4 KiB
/// page, or ending past [`DIRECT_MAP_WINDOW`], is refused.
pub fn direct_map_end(map: &[MemoryMapEntry]) -> Result<DirectMapEnd, Refusal> {
    map.iter().filter(|entry| is_read_as_memory(entry.uefi_type)).try_fold(0, |end: u64, entry| {
        if !entry.start.is_multiple_of(PAGE_4K) {
            return Err(Refusal::OffPage(entry.start));
        }
        if !entry.end.is_multiple_of(PAGE_4K) {
            return Err(Refusal::OffPage(entry.end));
        }
        if entry.end < entry.start {
            return Err(Refusal::Extent { base: entry.start, len: entry.end.wrapping_sub(entry.start) });
        }
        if entry.end > DIRECT_MAP_WINDOW {
            return Err(Refusal::PastWindow(entry.end));
        }
        Ok(end.max(entry.end.next_multiple_of(PAGE_2M)))
    })
    .map(DirectMapEnd)
}

/// Which of the 4 KiB pages of the 2 MiB page at `page` firmware's map calls
/// memory the kernel reads, over a map [`direct_map_end`] accepted.
pub fn coverage(map: &[MemoryMapEntry], page: u64) -> Coverage {
    let mut bits = [0u64; 8];
    for entry in map.iter().filter(|entry| is_read_as_memory(entry.uefi_type)) {
        let (low, high) = (entry.start.max(page), entry.end.min(page + PAGE_2M));
        for index in (low.saturating_sub(page) / PAGE_4K)..(high.saturating_sub(page) / PAGE_4K) {
            bits[(index / 64) as usize] |= 1 << (index % 64);
        }
    }
    match bits.iter().map(|word| u64::from(word.count_ones())).sum::<u64>() {
        0 => Coverage::Nothing,
        PAGES => Coverage::Whole,
        _ => Coverage::Pages(bits),
    }
}
