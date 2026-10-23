//! `SYS_VM_RESERVE` with words `toyos_abi::syscall::vm_reserve`'s types
//! cannot spell, so a test can ask with an undefined one.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use toyos_abi::syscall::{SyscallError, SYS_VM_RESERVE};

    /// The range's address, or the error the kernel refused it with.
    ///
    /// # Safety
    /// A fixed range replaces whatever this process has at `place`.
    pub unsafe fn raw(len: u64, prot: u64, place: u64, kind: u64) -> Result<u64, SyscallError> {
        let ret: u64;
        // SAFETY: the caller's for what a fixed range replaces; no argument
        // is a pointer this call dereferences, and the `syscall` is the ABI's
        // (`toyos_abi::syscall`), which clobbers rax, rcx and r11.
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rdi") SYS_VM_RESERVE,
                in("rsi") len,
                in("rdx") prot,
                in("r8") place,
                in("r9") kind,
                lateout("rax") ret,
                out("rcx") _,
                out("r11") _,
            );
        }
        SyscallError::from_u64(ret).map_or(Ok(ret), Err)
    }
}
