//! Writing memory back out of the caches, for the one reader that is not a
//! CPU: DRAM across a reset.

/// The smallest data cache line on this machine, from `CTR_EL0.DminLine`
/// (log2 of the word count), so a walk by it misses no line.
fn line() -> u64 {
    let ctr: u64;
    // SAFETY: reads `CTR_EL0`, which EL1 may always read.
    unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    4 << ((ctr >> 16) & 0xF)
}

/// Every line of `[at, at + len)` cleaned to the point of coherency and
/// invalidated before this returns: `DC CIVAC` by line, then `DSB SY` for
/// their completion.
///
/// A reset invalidates the caches without writing them back, so a byte that
/// must outlive one has to reach DRAM first.
pub fn write_back(at: u64, len: usize) {
    let step = line();
    let mut addr = at & !(step - 1);
    while addr < at + len as u64 {
        // SAFETY: `DC CIVAC` cleans and invalidates the line holding a mapped
        // address the caller owns; it changes no memory's contents.
        unsafe { core::arch::asm!("dc civac, {}", in(reg) addr, options(nostack, preserves_flags)) };
        addr += step;
    }
    // SAFETY: a barrier; waits for the maintenance above to complete.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}
