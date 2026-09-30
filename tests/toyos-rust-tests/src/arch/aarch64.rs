//! AArch64's half of the test binaries' assembly.

/// Syscall `number` with no argument set, past every `toyos_abi` wrapper; the
/// kernel's answer.
///
/// # Safety
/// The kernel refuses `number` without reading any argument register.
pub unsafe fn bare_syscall(number: u64) -> u64 {
    let answer: u64;
    // SAFETY: the caller's; the `svc` is the ABI's (`toyos_abi::syscall`),
    // which answers in x0 and keeps every other register.
    unsafe {
        core::arch::asm!("svc #0", inlateout("x0") number => answer);
    }
    answer
}
