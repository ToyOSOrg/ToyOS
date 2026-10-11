//! A number no syscall has is refused as `InvalidArgument`, on the kernel an
//! image ships.

use toyos_abi::syscall::SyscallError;

#[path = "../arch/mod.rs"]
mod arch;

fn main() {
    // SAFETY: a number the kernel refuses without reading any argument;
    // nothing in this process is touched.
    let answer = unsafe { arch::bare_syscall(u64::MAX) };
    assert_eq!(answer, SyscallError::InvalidArgument.to_u64(), "syscall {} answered {answer:#x}", u64::MAX);
}
