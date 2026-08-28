//! A machine whose bring-up saw a failed AP must still survive a TLB shootdown.
//!
//! This binary runs on the boot the harness arms with `smp-skip-ap`, which skips
//! the startup of the AP that would be cpu2 — a non-last AP that never starts.
//! On the unfixed kernel that spends cpu2's id and slot before the AP runs and
//! then counts a later AP anyway, so `0..cpu_count()` gains a dead slot: the
//! next shootdown waits on a CPU that never existed, hits the 5 s acknowledgement
//! tripwire, and panics — the machine dies and this binary's marker never prints.
//!
//! The verdict is survival. Each `munmap` frees the 2 MiB back to the PMM and
//! issues a shootdown to every id in `0..cpu_count()`; a machine that has one
//! real dead slot cannot answer it. The dense machine returns in microseconds
//! and prints the marker below.

use toyos_abi::syscall::{self, MmapFlags, MmapProt};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// Enough shootdowns that a boot which happened to skip one is not the reason it
/// survived; one dead slot fails the first of them.
const ROUNDS: usize = 8;

fn main() {
    for _ in 0..ROUNDS {
        let p = unsafe {
            syscall::mmap(
                core::ptr::null_mut(),
                PAGE_2M,
                MmapProt::READ | MmapProt::WRITE,
                MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
            )
        };
        assert!(!p.is_null(), "mmap failed");
        // Cache a translation for the range on this CPU before freeing it, so the
        // free is a real shootdown target rather than an untouched mapping.
        unsafe { core::ptr::write_volatile(p, 0xA5) };
        unsafe { syscall::munmap(p, PAGE_2M) }.expect("munmap");
    }
    println!("smp_hole_shootdown: survived {ROUNDS} shootdowns");
}
