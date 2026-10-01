//! A number no syscall has is refused as `InvalidArgument`, on the kernel an
//! image ships.

use toyos_abi::syscall::SyscallError;

#[path = "../arch/mod.rs"]
mod arch;

/// Numbers no syscall has: one inside the table's range and the last a
/// register holds.
const UNASSIGNED: [u64; 2] = [26, u64::MAX];

fn main() {
    for number in UNASSIGNED {
        // SAFETY: a number the kernel refuses without reading any argument;
        // nothing in this process is touched.
        let answer = unsafe { arch::bare_syscall(number) };
        assert_eq!(
            answer,
            SyscallError::InvalidArgument.to_u64(),
            "syscall {number} answered {:?}",
            SyscallError::from_u64(answer),
        );
    }
}
