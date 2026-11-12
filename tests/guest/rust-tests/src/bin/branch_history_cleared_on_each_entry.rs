//! Each entry from Ring 3 runs its CPU's branch history clear where the CPU's
//! decision names one, read off the kernel's count of clears through
//! `SYS_DEBUG`: at least one per syscall, through `LSTAR`'s entry, and one per
//! page fault on a fresh page, through the IDT's, since every other CPU's
//! entries only add to the count. On a CPU whose decision names none the
//! count is refused, and there is nothing to hold.

use toyos_abi::syscall::{self, debug_action, SyscallError, VmAt, VmProt};

const SYSCALLS: u64 = 1000;
const PAGES: usize = 256;
const PAGE: usize = 4096;

fn clears() -> u64 {
    syscall::debug(debug_action::BRANCH_HISTORY_CLEARS)
}

fn main() {
    let cpu = syscall::current_cpu();
    let mut mask = [0u64; syscall::CPU_MASK_WORDS];
    mask[cpu as usize / 64] = 1 << (cpu % 64);
    syscall::thread_set_affinity(0, &mask).expect("pinned to one CPU");
    let before = clears();
    if SyscallError::from_u64(before) == Some(SyscallError::NotSupported) {
        println!("branch_history_cleared_on_each_entry: CPU {cpu}'s decision names no branch history clear");
        return;
    }
    for _ in 0..SYSCALLS {
        syscall::current_cpu();
    }
    let by_syscalls = clears() - before;
    assert!(by_syscalls >= SYSCALLS, "CPU {cpu} cleared branch history {by_syscalls} times over {SYSCALLS} syscalls");

    // SAFETY: a fresh range nothing else names; committed, so each page is
    // populated by its first touch below, a Ring 3 page fault.
    let region = unsafe { syscall::vm_reserve(PAGES * PAGE, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("reserve");
    let before = clears();
    for page in 0..PAGES {
        // SAFETY: inside the range just reserved, which nothing else names.
        unsafe { region.add(page * PAGE).write_volatile(1) };
    }
    let by_faults = clears() - before;
    // SAFETY: the range reserved above, which nothing names any more.
    unsafe { syscall::vm_release(region, PAGES * PAGE) }.expect("release");
    assert!(by_faults >= PAGES as u64, "CPU {cpu} cleared branch history {by_faults} times over {PAGES} page faults");
    println!(
        "branch_history_cleared_on_each_entry: {by_syscalls} clears over {SYSCALLS} syscalls and {by_faults} over {PAGES} page faults on CPU {cpu}"
    );
}
