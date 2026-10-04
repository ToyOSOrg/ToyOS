//! `SYS_MMAP` past `toyos_abi::syscall::mmap`, which answers every refusal with
//! a null: here the kernel's error comes back by name.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use toyos_abi::syscall::{MmapFlags, MmapProt, SyscallError, SYS_MMAP};

    /// The mapping's address, or the error the kernel refused it with.
    ///
    /// # Safety
    /// A `FIXED` mapping replaces whatever this process has at `addr`.
    pub unsafe fn raw(addr: u64, len: u64, prot: MmapProt, flags: MmapFlags) -> Result<u64, SyscallError> {
        let ret: u64;
        // SAFETY: the caller's for what a `FIXED` mapping replaces; no argument
        // is a pointer this call dereferences, and the `syscall` is the ABI's
        // (`toyos_abi::syscall`), which clobbers rax, rcx and r11.
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rdi") SYS_MMAP,
                in("rsi") addr,
                in("rdx") len,
                in("r8") prot.0,
                in("r9") flags.0,
                lateout("rax") ret,
                out("rcx") _,
                out("r11") _,
            );
        }
        SyscallError::from_u64(ret).map_or(Ok(ret), Err)
    }
}
