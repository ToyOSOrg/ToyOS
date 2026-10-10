//! x86-64's encoding of a [`Plan`](crate::Plan): Intel SDM Vol. 3A §4.5,
//! Tables 4-15 (a PML4 or PDPT entry naming a table) and 4-17 (a page
//! directory entry mapping a 2 MiB page); and how far the kernel's own direct
//! map reaches.

use toyos_abi::boot::MemoryMapEntry;

use crate::{is_read_as_memory, Cache, DirectMapEnd, Encoding, Refusal, BOOT_MAP_BYTES, DIRECT_MAP_WINDOW, PAGE_2M};

/// One past the kernel direct map's last byte: [`BOOT_MAP_BYTES`], or the end
/// of the highest range the kernel reads as memory in whole [`PAGE_2M`] pages,
/// whichever is higher. Memory past [`DIRECT_MAP_WINDOW`] is refused.
///
/// The low [`BOOT_MAP_BYTES`] are mapped whole, registers and holes included,
/// because x86-64 types those pages by its MTRRs rather than by the entry, and
/// because the kernel goes on using addresses it took through the boot map.
pub fn direct_map_end(map: &[MemoryMapEntry]) -> Result<DirectMapEnd, Refusal> {
    map.iter().filter(|entry| is_read_as_memory(entry.uefi_type)).try_fold(BOOT_MAP_BYTES, |end, entry| {
        if entry.end > DIRECT_MAP_WINDOW {
            return Err(Refusal::PastWindow(entry.end));
        }
        Ok(end.max(entry.end.next_multiple_of(PAGE_2M)))
    })
    .map(DirectMapEnd)
}

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
/// Page-level write-through, bit 3.
const PWT: u64 = 1 << 3;
/// Page-level cache disable, bit 4.
const PCD: u64 = 1 << 4;
/// A page directory entry that maps 2 MiB rather than naming a table, bit 7.
const PAGE_SIZE: u64 = 1 << 7;

/// An entry naming the next table down, at `phys`.
pub const fn table(phys: u64) -> u64 {
    phys | PRESENT | WRITABLE
}

/// A 2 MiB leaf at `phys`. Memory and the MTRR-typed page select PAT entry 0,
/// whose type is the MTRR's; a device and the scanout select entry 3 (PCD and
/// PWT with the PAT bit clear), uncacheable under every MTRR type and under an
/// unprogrammed PAT, which is what the machine has until the kernel's
/// `pat::init`.
pub const fn block(phys: u64, cache: Cache) -> u64 {
    let typed = match cache {
        Cache::Firmware | Cache::Memory => 0,
        Cache::Device | Cache::Scanout => PCD | PWT,
    };
    phys | PRESENT | WRITABLE | PAGE_SIZE | typed
}

/// A 4 KiB leaf at `phys` (Table 4-19), typed as [`block`] types a 2 MiB one;
/// a 4 KiB entry's PAT bit is bit 7, and it stays clear.
pub const fn page(phys: u64, cache: Cache) -> u64 {
    block(phys, cache) & !PAGE_SIZE
}

/// The three, for [`Plan::write`](crate::Plan::write).
pub const ENCODING: Encoding = Encoding { table, block, page };
