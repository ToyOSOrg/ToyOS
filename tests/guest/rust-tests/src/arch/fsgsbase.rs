//! FS and GS bases as Ring 3 writes them under `CR4.FSGSBASE`.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// This thread's GS base.
    pub fn gs_base() -> u64 {
        let base: u64;
        // SAFETY: reads GS's base alone.
        unsafe { core::arch::asm!("rdgsbase {}", out(reg) base, options(nomem, nostack, preserves_flags)) };
        base
    }

    /// Writes `fs` and `gs` as this thread's bases, makes syscall `yield_number`,
    /// and reads both back; the FS base it found is back before it returns, so
    /// nothing here runs on another thread-local block. The bases it read.
    pub fn bases_across_a_yield(fs: u64, gs: u64, yield_number: u64) -> (u64, u64) {
        let (fs_read, gs_read): (u64, u64);
        // SAFETY: both bases canonical, as `WRFSBASE` and `WRGSBASE` require;
        // nothing between the writes and the restore addresses FS or GS, and
        // the `syscall` clobbers rax, rcx and r11.
        unsafe {
            core::arch::asm!(
                "rdfsbase {kept}",
                "wrfsbase {fs}",
                "wrgsbase {gs}",
                "syscall",
                "rdfsbase {fs_read}",
                "rdgsbase {gs_read}",
                "wrfsbase {kept}",
                kept = out(reg) _,
                fs = in(reg) fs,
                gs = in(reg) gs,
                fs_read = out(reg) fs_read,
                gs_read = out(reg) gs_read,
                in("rdi") yield_number,
                lateout("rax") _,
                out("rcx") _,
                out("r11") _,
            );
        }
        (fs_read, gs_read)
    }
}
