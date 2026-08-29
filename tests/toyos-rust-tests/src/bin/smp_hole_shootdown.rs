//! Survives a TLB shootdown on the `smp-skip-ap` boot, where a non-last AP never
//! starts. The unfixed kernel leaves a dead slot in `0..cpu_count()` and the next
//! shootdown waits on a CPU that never existed and panics; the dense machine
//! returns and this marker prints. Each `munmap` frees 2 MiB back and shoots down.

use toyos_abi::syscall::{self, MmapFlags, MmapProt};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// Enough that a boot which happened to skip one shootdown is not why it survived.
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
        // Cache a translation before freeing, so the free is a real shootdown target.
        unsafe { core::ptr::write_volatile(p, 0xA5) };
        unsafe { syscall::munmap(p, PAGE_2M) }.expect("munmap");
    }
    println!("smp_hole_shootdown: survived {ROUNDS} shootdowns");
}
