//! x86-64's half of `unmap_touch`; its FP and first-entry probes are
//! elsewhere, or owed.

pub mod fp_isolation {
    pub fn main(_args: Vec<String>) {
        panic!("fp_isolation probes AArch64's FP/SIMD switch; x86-64's is test_rs_fpu_isolation");
    }
}

pub mod first_entry {
    pub fn main(_args: Vec<String>) {
        panic!(
            "first_entry probes AArch64's first entry to EL0; x86-64's is owed by \
             issues/isolation/a-new-x86-thread-enters-ring-3-holding-kernel-register-values.md"
        );
    }
}

/// Reads the word at `page`, unmaps the `len` bytes there with `SYS_MUNMAP`
/// and reads the word again, in one block, so nothing but the unmap itself
/// can switch the CPU between the reads. Answers the first read, the unmap's
/// answer and the second read.
///
/// # Safety
/// `page` starts a mapping of `len` bytes that nothing else names. The second
/// read ends the process unless the unmap was refused or left its
/// translation standing.
pub unsafe fn read_unmap_read(page: *const u64, len: usize) -> (u64, u64, u64) {
    let (first, answer, second): (u64, u64, u64);
    // SAFETY: the caller's; the two loads name `page` and the `syscall` is
    // the ABI's (`toyos_abi::syscall`), which clobbers rax, rcx and r11.
    unsafe {
        core::arch::asm!(
            "mov {first}, qword ptr [{page}]",
            "syscall",
            "mov {second}, qword ptr [{page}]",
            page = in(reg) page,
            first = out(reg) first,
            second = lateout(reg) second,
            in("rdi") toyos_abi::syscall::SYS_MUNMAP,
            in("rsi") page,
            in("rdx") len,
            in("r8") 0u64,
            in("r9") 0u64,
            out("rax") answer,
            out("rcx") _,
            out("r11") _,
        );
    }
    (first, answer, second)
}
