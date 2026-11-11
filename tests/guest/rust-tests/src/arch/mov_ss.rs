//! A trap a `MOV SS` holds back, with the instruction after it a syscall.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// Sets `RFLAGS.TF`, loads SS with its own selector, and makes syscall
    /// `number` with no argument; its answer. The single-step trap the `MOV
    /// SS` would take is suppressed (SDM Vol. 3A §7.8.3) and `SYSCALL` takes
    /// none, so the trap comes after the instruction the return lands on, and
    /// nothing here returns where the kernel is correct.
    pub fn single_step_over_mov_ss_into_syscall(number: u64) -> u64 {
        let answer: u64;
        // SAFETY: a balanced `pushfq`/`popfq` around one `or`, SS reloaded with
        // the selector it holds, and the ABI's `syscall`, which clobbers rax,
        // rcx and r11.
        unsafe {
            core::arch::asm!(
                "mov {ss:x}, ss",
                "pushfq",
                "or qword ptr [rsp], 0x100",
                "popfq",
                "mov ss, {ss:x}",
                "syscall",
                ss = out(reg) _,
                in("rdi") number,
                lateout("rax") answer,
                out("rcx") _,
                out("r11") _,
            );
        }
        answer
    }
}
