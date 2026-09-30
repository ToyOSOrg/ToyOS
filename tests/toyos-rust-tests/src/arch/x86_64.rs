//! x86-64's half of the test binaries' assembly.

/// Syscall `number` with no argument set, past every `toyos_abi` wrapper; the
/// kernel's answer.
///
/// # Safety
/// The kernel refuses `number` without reading any argument register.
pub unsafe fn bare_syscall(number: u64) -> u64 {
    let answer: u64;
    // SAFETY: the caller's; the `syscall` is the ABI's (`toyos_abi::syscall`),
    // which clobbers rax, rcx and r11.
    unsafe {
        core::arch::asm!("syscall", in("rdi") number, lateout("rax") answer, out("rcx") _, out("r11") _);
    }
    answer
}
