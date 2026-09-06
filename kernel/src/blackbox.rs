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

use toyos_blackbox::{BYTES, Fault, State, TEXT_BYTES};

use crate::mm::{DirectMap, Region};

/// Where the page is in the kernel's own view of memory, or 0 for a boot that
/// has none. Written once on the BSP before any AP exists, so a relaxed load is
/// the whole of the ordering the panic path needs.
static PAGE: AtomicU64 = AtomicU64::new(0);

/// The page's physical address, kept apart from [`PAGE`] because `mm::init`
/// wants the physical one and the writers want the mapped one.
static PHYS: AtomicU64 = AtomicU64::new(0);

/// The physical page `mm::init` must keep out of the allocator, or an empty
/// region where there is none.
///
/// Its result goes into the same array as the AP trampoline's page: both are
/// addresses this kernel was given rather than ones it allocated.
pub fn reserved_region() -> Region {
    match PHYS.load(Relaxed) {
        0 => Region { start: 0, end: 0 },
        at => Region { start: at, end: at.saturating_add(BYTES as u64) },
    }
}

/// Take the page the loader named, out of the raw parameter buffer.
///
/// **Called in `kernel_main`'s first statements, beside `panic_console::arm`
/// and before anything that can fail.** The panics this page exists for are the
/// early ones — the owner's laptop rendered a panel and then reset itself with
/// the page still reading the loader's own `ARMED`, because this ran after the
/// console, the parameter line's UTF-8 check and `params::init`, and the panic
/// was before all three. Bytes, not `&str`, for the same reason: nothing has
/// decided the buffer is UTF-8 yet, and that decision panics.
///
/// A boot whose loader claimed no page says so on the panel here, which is the
/// only place it can be said — the seal itself runs where nothing may log.
pub fn arm(cmdline: &[u8]) {
    let Some(at) = toyos_blackbox::address_in(cmdline) else {
        log!(
            "black box: this boot's parameter line names no page, so a panic reaches the panel \
             and nowhere else"
        );
        return;
    };
    PHYS.store(at, Relaxed);
    PAGE.store(DirectMap::from_phys(at).as_mut_ptr::<u8>() as u64, Relaxed);
    log!("black box: {at:#x} is this boot's, {TEXT_BYTES} bytes for the next boot's loader");
}

/// Seal an exception's registers before anything else in the handler runs.
///
/// **First, and before any lock, allocation, panel write, symbol lookup or
/// format.** The owner's laptop reset itself with the page still holding the
/// loader's `ARMED` after a panic it had rendered on screen — a handler that
/// faults again becomes a double and then a triple fault, and a triple fault is
/// a reset with nothing written. What the entry can say without risking a second
/// fault is its own registers, so it says those first and the report overwrites
/// them if it ever gets that far.
pub fn record_fault(fault: &Fault) {
    seal(State::Fault, &fault.to_bytes());
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
        // **Refused, and silently, because this is the one site that cannot
        // speak**: `record_panic` runs inside `panic_console::render`, which may
        // take no lock and re-enter nothing. What a boot with no page loses is
        // said at `arm`, on the panel, while there is still a machine to say it on.
        return;
    }
    // SAFETY: `at` is non-zero only where `arm` found the address on this
    // boot's parameter line, which is the page the loader allocated and which
    // `mm::init` was told to keep out of the allocator. The panic path holds
    // `PAINTING` and the quiesce path has stopped every other CPU, so the one
    // CPU still running is the only writer.
    let page = unsafe { &mut *(at as *mut [u8; BYTES]) };
    toyos_blackbox::seal(page, state, text);
    flush(at);
}

/// One cache line per `CLFLUSH`, which is what the page has to be written back in.
const CACHE_LINE: u64 = 64;

/// Write the page out of this CPU's caches, and every other CPU's.
///
/// **A reset does not write dirty lines back.** INIT and RESET invalidate the
/// caches without flushing them (SDM Vol. 3A §11.5.3 on cache invalidation
/// across a reset), so a page sealed into write-back memory and then reset over
/// is a page whose bytes never reached DRAM — which is the one failure this
/// whole mechanism cannot survive, and it looks exactly like a seal that never
/// happened. `CLFLUSH` is coherent across every CPU in the machine, so one
/// caller's flush is the whole machine's.
fn flush(at: u64) {
    let mut line = 0u64;
    while line < BYTES as u64 {
        // SAFETY: `CLFLUSH` writes back and invalidates the line containing the
        // address and touches nothing else; the address is inside the page
        // `arm` took, and the instruction faults on nothing a canonical address
        // can be. Not privileged, and present on every x86-64 part.
        unsafe {
            core::arch::asm!(
                "clflush [{addr}]",
                addr = in(reg) (at + line) as *const u8,
                options(nostack, preserves_flags),
            );
        }
        line += CACHE_LINE;
    }
    // SAFETY: `SFENCE` orders those writebacks ahead of whatever ends this
    // machine; it touches no memory or register.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}
