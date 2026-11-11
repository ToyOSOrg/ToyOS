//! A single-step trap over an instruction that enters the kernel, held back by
//! a `MOV SS` or not.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    /// Sets `RFLAGS.TF`, loads SS with its own selector, and makes syscall
    /// `number` with no argument; its answer. Intel suppresses the single-step
    /// trap the `MOV SS` would take (SDM Vol. 3A §7.8.3), and AMD holds it until
    /// the `SYSCALL` completes (APM Vol. 2 §8.1.4), on the entry's first
    /// instruction. Either way the trap the return's `TF` arms ends the thread
    /// at the instruction after it, so nothing here returns where the kernel
    /// is correct.
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

    /// The same `TF` and `MOV SS`, with `INT3` after them: a trap the `MOV SS`
    /// held lands on the `#BP` gate's first instruction at CPL 0, with Ring 3's
    /// GS base live and a Ring 0 frame. The `#BP` ends the thread as a trap;
    /// nothing here returns.
    pub fn single_step_over_mov_ss_into_int3() {
        // SAFETY: as `single_step_over_mov_ss_into_syscall`, with `INT3`, which
        // reads and writes nothing, in place of the syscall.
        unsafe {
            core::arch::asm!(
                "mov {ss:x}, ss",
                "pushfq",
                "or qword ptr [rsp], 0x100",
                "popfq",
                "mov ss, {ss:x}",
                "int3",
                ss = out(reg) _,
            );
        }
    }

    /// The same `TF` and `MOV SS`, with `INT1` after them: `INT1`'s own `#DB`
    /// takes the `#DB` stack from Ring 3, and a trap the `MOV SS` held may
    /// land on that entry's first instruction: AMD documents the delay (APM
    /// Vol. 2 §8.1.4), not whether `INT1`'s own `#DB` absorbs it. The thread
    /// ends as a trap; nothing here returns.
    pub fn single_step_over_mov_ss_into_int1() {
        // SAFETY: as `single_step_over_mov_ss_into_int3`; `.byte 0xf1` is
        // `INT1`, which this toolchain's assembler does not spell.
        unsafe {
            core::arch::asm!(
                "mov {ss:x}, ss",
                "pushfq",
                "or qword ptr [rsp], 0x100",
                "popfq",
                "mov ss, {ss:x}",
                ".byte 0xf1",
                ss = out(reg) _,
            );
        }
    }

    /// `TF` set, then `INT1` with no `MOV SS`: a single-step trap pending
    /// after `INT1`'s delivery through the IDT, which nothing documents
    /// against, would land on the `#DB` entry's first instruction. The
    /// thread ends as a trap; nothing here returns.
    pub fn single_step_into_int1() {
        // SAFETY: a balanced `pushfq`/`popfq` around one `or`, then `INT1`,
        // which reads and writes nothing.
        unsafe {
            core::arch::asm!(
                "pushfq",
                "or qword ptr [rsp], 0x100",
                "popfq",
                ".byte 0xf1",
            );
        }
    }
}
