//! The kernel's half of the panic black box: one page of DRAM that outlives the
//! reset, holding what the panel rendered for the next boot's loader to find.
//!
//! **This kernel writes that page only where the memory map says the loader
//! claimed it.** The claim is a UEFI memory type in the range reserved for OS
//! loaders, so firmware cannot have produced one and an entry of that type at
//! the address both binaries name came from our loader and from nothing else.
//! There is no flag and no `KernelArgs` field to get wrong: the map says the
//! page is this kernel's, or the panic goes nowhere but the panel.
//!
//! The same type is what keeps the page out of the PMM, asserted below rather
//! than assumed — a black box the allocator can hand to a process is a page
//! that holds somebody's data at the moment a panic overwrites it.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use toyos_abi::boot::MemoryMapEntry;
use toyos_blackbox::{BYTES, MEMORY_TYPE, PHYS, TEXT_BYTES};

use crate::mm::DirectMap;

/// A page the PMM would hand out is not a black box.
const _: () = assert!(!crate::mm::pmm::is_usable_type(MEMORY_TYPE));

/// Where the page is in the kernel's own view of memory, or 0 for a boot that
/// has none. Written once on the BSP before any AP exists, so a relaxed load is
/// the whole of the ordering the panic path needs.
static PAGE: AtomicU64 = AtomicU64::new(0);

/// Take the page if the loader claimed it, from `KernelArgs`' memory map.
///
/// Runs beside `panic_console::arm`, before `mm::init`, and needs no counterpart
/// after it: the address is ordinary DRAM, which the boot map covers at
/// `PHYS_OFFSET` in this window and the direct map covers for the rest of the
/// boot. The scanout needs a remap because it is device memory; this does not.
pub fn arm(maps: &[MemoryMapEntry]) {
    let end = PHYS.saturating_add(BYTES as u64);
    let claimed = maps
        .iter()
        .any(|entry| entry.uefi_type == MEMORY_TYPE && entry.start <= PHYS && entry.end >= end);
    if !claimed {
        log!(
            "black box: no {MEMORY_TYPE:#x} page at {PHYS:#x} in the memory map, so a panic on \
             this boot reaches the panel and nowhere else"
        );
        return;
    }
    PAGE.store(DirectMap::from_phys(PHYS).as_mut_ptr::<u8>() as u64, Relaxed);
    log!("black box: {PHYS:#x} is this boot's, {TEXT_BYTES} bytes for the next boot's loader");
}

/// Seal `text` into the page for the next boot.
///
/// Called from `panic_console::render`, inside the region that may take no
/// lock, allocate nothing and panic nowhere: one bounded copy into a page
/// nothing else in this machine names.
pub fn record(text: &[u8]) {
    let at = PAGE.load(Relaxed);
    if at == 0 {
        return;
    }
    // SAFETY: `at` is non-zero only where `arm` found the loader's own claim on
    // this page in the memory map, which is what makes it neither firmware's
    // nor the PMM's (asserted at the top of this file). The panic path holds
    // `PAINTING`, so the one CPU still running is the only writer, and the
    // pointer is a `[u8; BYTES]` at an address that page was allocated for.
    let page = unsafe { &mut *(at as *mut [u8; BYTES]) };
    toyos_blackbox::seal(page, text);
}
