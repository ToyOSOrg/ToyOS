//! Writing memory back out of the caches, for the one reader that is not a
//! CPU: DRAM across a reset.

/// `CLFLUSH`'s line: `CPUID.01H:EBX[15:8] × 8`, which every x86-64 part
/// reports as 64.
const LINE: u64 = 64;

/// Every line of `[at, at + len)` written back to memory and invalidated, in
/// this CPU's caches and every other CPU's — `CLFLUSH` is coherent across the
/// machine — before this returns.
///
/// A reset invalidates the caches without writing them back, so a byte that
/// must outlive one has to reach DRAM first.
pub fn write_back(at: u64, len: usize) {
    let mut line = at & !(LINE - 1);
    while line < at + len as u64 {
        // SAFETY: `CLFLUSH` writes back and invalidates the line containing the
        // address and touches nothing else; the caller names memory it owns,
        // and the instruction faults on nothing a mapped canonical address can
        // be. Not privileged, and present on every x86-64 part.
        unsafe {
            core::arch::asm!(
                "clflush [{addr}]",
                addr = in(reg) line as *const u8,
                options(nostack, preserves_flags),
            );
        }
        line += LINE;
    }
    // SAFETY: `SFENCE` orders those writebacks ahead of whatever ends this
    // machine; it touches no memory or register.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}
