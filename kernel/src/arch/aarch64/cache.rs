//! Cache maintenance, for the two readers that are not this CPU's data side:
//! DRAM across a reset, and the instruction stream.

/// `CTR_EL0`, which EL1 may always read.
fn ctr() -> u64 {
    let ctr: u64;
    // SAFETY: reads `CTR_EL0`, which EL1 may always read.
    unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    ctr
}

/// The smallest data cache line on this machine, from `CTR_EL0.DminLine`
/// (log2 of the word count), so a walk by it misses no line.
fn line() -> u64 {
    4 << ((ctr() >> 16) & 0xF)
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

/// Instructions this CPU wrote through the data side at `[at, at + len)` made
/// the ones every CPU fetches: the data cleaned to the point of unification
/// unless `CTR_EL0.IDC` says it need not be, then every instruction cache
/// invalidated unless `CTR_EL0.DIC` says the same (Arm ARM K.a, D7.5.9.2).
/// Before a mapping that executes them is written.
pub fn make_executable(at: u64, len: usize) {
    let ctr = ctr();
    if ctr >> 28 & 1 == 0 {
        let step = line();
        let mut addr = at & !(step - 1);
        while addr < at + len as u64 {
            // SAFETY: `DC CVAU` cleans the line holding a mapped address the
            // caller owns; it changes no memory's contents.
            unsafe { core::arch::asm!("dc cvau, {}", in(reg) addr, options(nostack, preserves_flags)) };
            addr += step;
        }
        // SAFETY: a barrier; waits for the cleaning above to complete.
        unsafe { core::arch::asm!("dsb ish", options(nostack, preserves_flags)) };
    }
    if ctr >> 29 & 1 == 0 {
        // SAFETY: invalidating instruction caches changes no memory; the
        // barrier waits for it on every CPU.
        unsafe { core::arch::asm!("ic ialluis", "dsb ish", options(nostack, preserves_flags)) };
    }
    // SAFETY: a context synchronization; touches nothing.
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}
