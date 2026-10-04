//! A process's regions are bounded at `MAX_REGIONS`: past it every way a
//! mapping enters the address space is refused by name, a replacement — which
//! adds no region — is not, and one region freed is room for one.

#[path = "../arch/mmap.rs"]
mod mmap;

use toyos_abi::syscall::{munmap, MmapFlags, MmapProt, SyscallError, MAX_REGIONS};
use SyscallError::ResourceExhausted;

const PAGE_2M: u64 = 2 * 1024 * 1024;

/// The process's own regions — its segments, stack, TLS block and heap — that
/// the fill below never gets.
const OWN_REGIONS: usize = 64;

/// The fill's first mappings, kept by address: one replaced and one freed at
/// the bound, the rest freed before anything is judged.
const KEPT: usize = 8;

fn unmap(addr: u64) {
    unsafe { munmap(addr as *mut u8, PAGE_2M as usize) }
        .unwrap_or_else(|e| panic!("unmapping {addr:#x} was refused: {e:?}"));
}

fn main() {
    let anywhere = MmapFlags::ANONYMOUS | MmapFlags::PRIVATE;
    let placed = anywhere | MmapFlags::FIXED;
    let rw = MmapProt::READ | MmapProt::WRITE;

    let mut count = 0usize;
    let mut kept = [0u64; KEPT];
    let mut lowest = u64::MAX;
    let mut refusal = None;
    for _ in 0..=MAX_REGIONS {
        // SAFETY: not `FIXED`, so nothing of this process's is replaced.
        match unsafe { mmap::raw(0, PAGE_2M, MmapProt::NONE, anywhere) } {
            Ok(addr) => {
                if let Some(slot) = kept.get_mut(count) {
                    *slot = addr;
                }
                lowest = lowest.min(addr);
                count += 1;
            }
            Err(e) => {
                refusal = Some(e);
                break;
            }
        }
    }
    let refusal = refusal.unwrap_or_else(|| {
        panic!("{count} PROT_NONE mappings were all placed: the region count is unbounded")
    });

    // Placement runs top-down, so nothing is registered below the fill.
    let free = lowest - 2 * PAGE_2M;
    // SAFETY: a `FIXED` mapping lands at `free`, where nothing is, or over
    // `kept[0]`, which this process reads nothing through.
    let (placed_free, anywhere_rw, replaced) = unsafe {
        (
            mmap::raw(free, PAGE_2M, MmapProt::NONE, placed),
            mmap::raw(0, PAGE_2M, rw, anywhere),
            mmap::raw(kept[0], PAGE_2M, rw, placed),
        )
    };
    unmap(kept[1]);
    // SAFETY: as above.
    let (placed_free_again, anywhere_rw_again) =
        unsafe { (mmap::raw(free, PAGE_2M, MmapProt::NONE, placed), mmap::raw(0, PAGE_2M, rw, anywhere)) };
    placed_free_again.into_iter().chain(anywhere_rw_again).for_each(unmap);
    // SAFETY: not `FIXED`.
    let served = unsafe { mmap::raw(0, PAGE_2M, rw, anywhere) };
    // Room for this process's own heap, which a failed assertion's message needs.
    served.into_iter().chain(kept[2..].iter().copied()).for_each(unmap);

    assert_eq!(refusal, ResourceExhausted, "the mapping after {count} was refused for another reason");
    assert!(
        count > MAX_REGIONS - OWN_REGIONS,
        "refused after {count} mappings, short of {MAX_REGIONS} by more than this process's own regions",
    );
    assert_eq!(placed_free, Err(ResourceExhausted), "a placed mapping at a free address was not refused at the bound");
    assert_eq!(anywhere_rw, Err(ResourceExhausted), "an RW mapping was not refused at the bound");
    assert_eq!(
        replaced,
        Ok(kept[0]),
        "a placed mapping over one of this process's own was refused at the bound, though it adds no region",
    );
    assert_eq!(placed_free_again, Ok(free), "the placed mapping found no room a free region made");
    assert_eq!(anywhere_rw_again, Err(ResourceExhausted), "an RW mapping was placed past the bound");
    assert!(served.is_ok(), "an RW mapping found no room a free region made: {served:?}");

    println!("{count} mappings placed and the next refused; at the bound a replacement maps, and a free region is room for one");
}
