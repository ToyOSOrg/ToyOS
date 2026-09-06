//! The kernel's half of the boot chain's black box: one page of DRAM that
//! outlives a reset, and the two things this kernel writes into it.
//!
//! The loader claims the page, seals `ARMED` into it and hands the machine over.
//! From here there are exactly two ways for that to be replaced: the panic path
//! seals what the panel rendered ([`record_panic`]), and the deliberate handover
//! back to firmware seals that it was deliberate ([`record_done`]). A page that
//! still reads `ARMED` on the next boot is a kernel that reached neither, which
//! is the finding the next loader reports.
//!
//! **Where the page is comes off the parameter line and out of no map.** An
//! ordinary `LoaderData` allocation is indistinguishable in the memory map from
//! every other one, so the loader says the address instead — and a boot whose
//! claim firmware refused simply carries no such word, which is how this kernel
//! knows it has no page rather than inferring one.
//!
//! `LoaderData` is memory the allocator would otherwise hand out, so
//! `kernel_main` reserves this page before `mm::init` takes the map. A black box
//! a process can be given is a page holding somebody's data at the moment a
//! panic overwrites it.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use toyos_blackbox::{BYTES, State, TEXT_BYTES};

use crate::mm::{DirectMap, Region};

/// Where the page is in the kernel's own view of memory, or 0 for a boot that
/// has none. Written once on the BSP before any AP exists, so a relaxed load is
/// the whole of the ordering the panic path needs.
static PAGE: AtomicU64 = AtomicU64::new(0);

/// The physical page `mm::init` must keep out of the allocator, or an empty
/// region where there is none.
///
/// Its result goes into the same array as the AP trampoline's page: both are
/// addresses this kernel was given rather than ones it allocated.
pub fn reserved_region() -> Region {
    match crate::params::blackbox_page() {
        Some(at) => Region { start: at, end: at.saturating_add(BYTES as u64) },
        // Empty, and `overlaps_reserved` reads it as covering nothing.
        None => Region { start: 0, end: 0 },
    }
}

/// Take the page the loader named, after `mm::init` has kept it out of the
/// allocator. A boot with no page says so and writes nowhere.
pub fn arm() {
    let Some(at) = crate::params::blackbox_page() else {
        log!(
            "black box: this boot's loader claimed no page, so a panic reaches the panel and \
             nowhere else"
        );
        return;
    };
    PAGE.store(DirectMap::from_phys(at).as_mut_ptr::<u8>() as u64, Relaxed);
    log!("black box: {at:#x} is this boot's, {TEXT_BYTES} bytes for the next boot's loader");
}

/// Seal what the panel rendered, for the next boot to report.
///
/// Called from `panic_console::render`, inside the region that may take no
/// lock, allocate nothing and panic nowhere: one bounded copy into a page
/// nothing else in this machine names.
pub fn record_panic(text: &[u8]) {
    seal(State::Panic, text);
}

/// Seal that this kernel handed the machine back on purpose, so the next
/// loader ends the chain instead of reporting a death.
///
/// Called from the quiesce path before the reset, which is the last point at
/// which this kernel is still the one running.
pub fn record_done() {
    seal(State::Done, &[]);
}

fn seal(state: State, text: &[u8]) {
    let at = PAGE.load(Relaxed);
    if at == 0 {
        return;
    }
    // SAFETY: `at` is non-zero only where `arm` found the address on this
    // boot's parameter line, which is the page the loader allocated and which
    // `mm::init` was told to keep out of the allocator. The panic path holds
    // `PAINTING` and the quiesce path has stopped every other CPU, so the one
    // CPU still running is the only writer.
    let page = unsafe { &mut *(at as *mut [u8; BYTES]) };
    toyos_blackbox::seal(page, state, text);
}
