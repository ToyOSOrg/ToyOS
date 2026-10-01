//! A number no syscall has is refused as `InvalidArgument`, on the kernel an
//! image ships.

use toyos_abi::syscall::SyscallError;

#[path = "../arch/mod.rs"]
mod arch;

/// Numbers no syscall has: one inside the table's range and the last a
/// register holds.
const UNASSIGNED: [u64; 2] = [26, u64::MAX];

fn main() {
    // SAFETY: a number the kernel refuses without reading any argument;
    // nothing in this process is touched.
    let answers = UNASSIGNED.map(|number| (number, unsafe { arch::bare_syscall(number) }));
    // Every number's answer in the one verdict, so a red names each of them.
    assert!(
        answers.iter().all(|&(_, answer)| answer == SyscallError::InvalidArgument.to_u64()),
        "answered {:?}",
        answers.map(|(number, answer)| (number, SyscallError::from_u64(answer))),
    );
}
