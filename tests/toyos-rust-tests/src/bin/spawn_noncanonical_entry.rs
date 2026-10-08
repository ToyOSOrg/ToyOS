//! A thread entry outside the user half is refused by name, before any thread
//! exists to return to it.

use toyos_abi::syscall::{self, MmapFlags, MmapProt, SyscallError};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// Canonical under neither 48-bit nor 57-bit addressing; the first address
/// past the user half; the first of the kernel half.
const ENTRIES: [u64; 3] = [0x0100_0000_0000_0000, 0x0000_8000_0000_0000, 0xFFFF_8000_0000_0000];

fn main() {
    // SAFETY: a fresh anonymous mapping nothing else names.
    let stack = unsafe {
        syscall::mmap(
            core::ptr::null_mut(),
            PAGE_2M,
            MmapProt::READ | MmapProt::WRITE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!stack.is_null(), "no memory for the thread's stack");
    let base = stack as u64;
    for entry in ENTRIES {
        // SAFETY: the stack is the mapping above; the entry is the input under test.
        let answer = unsafe { syscall::thread_spawn(entry, base + PAGE_2M as u64, 0, base) };
        if SyscallError::from_u64(answer).is_none() {
            // Accepted: the thread's first return to Ring 3 is where the harm is, so wait it out before judging.
            syscall::thread_join(answer);
        }
        assert_eq!(
            SyscallError::from_u64(answer),
            Some(SyscallError::InvalidArgument),
            "a thread entry of {entry:#x} was not refused",
        );
    }
}
