//! GS as a Ring 3 thread loads it and reads through it.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// Loads `selector` into GS, which loads its descriptor's base with it (SDM
    /// Vol. 2B, MOV). A selector Ring 3 may not load is `#GP`.
    pub fn load_gs(selector: u16) {
        // SAFETY: writes GS alone, which nothing in this program addresses
        // through but [`read_through_gs`].
        unsafe { core::arch::asm!("mov gs, {0:x}", in(reg) selector, options(nomem, nostack, preserves_flags)) };
    }

    /// The eight bytes at `offset` from GS's base.
    ///
    /// # Safety
    /// GS's base plus `offset` is eight readable bytes.
    pub unsafe fn read_through_gs(offset: u64) -> u64 {
        let word: u64;
        // SAFETY: the caller's.
        unsafe {
            core::arch::asm!(
                "mov {word}, qword ptr gs:[{offset}]",
                word = out(reg) word,
                offset = in(reg) offset,
                options(nostack, readonly, preserves_flags),
            );
        }
        word
    }
}
