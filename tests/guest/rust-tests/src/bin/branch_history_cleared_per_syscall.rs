//! Each syscall this job makes runs its CPU's branch history clear where the
//! CPU's decision names one, read off the kernel's count of clears through
//! `SYS_DEBUG`: at least one per syscall, since every other CPU's entries
//! only add to it. On a CPU whose decision names none the count is refused,
//! and there is nothing to hold.

use toyos_abi::syscall::{self, debug_action, SyscallError};

const SYSCALLS: u64 = 1000;

fn main() {
    let cpu = syscall::current_cpu();
    let mut mask = [0u64; syscall::CPU_MASK_WORDS];
    mask[cpu as usize / 64] = 1 << (cpu % 64);
    syscall::thread_set_affinity(0, &mask).expect("pinned to one CPU");
    let before = syscall::debug(debug_action::BRANCH_HISTORY_CLEARS);
    if SyscallError::from_u64(before) == Some(SyscallError::NotSupported) {
        println!("branch_history_cleared_per_syscall: CPU {cpu}'s decision names no branch history clear");
        return;
    }
    for _ in 0..SYSCALLS {
        syscall::current_cpu();
    }
    let cleared = syscall::debug(debug_action::BRANCH_HISTORY_CLEARS) - before;
    assert!(cleared >= SYSCALLS, "CPU {cpu} cleared branch history {cleared} times over {SYSCALLS} syscalls");
    println!("branch_history_cleared_per_syscall: {cleared} clears over {SYSCALLS} syscalls on CPU {cpu}");
}
