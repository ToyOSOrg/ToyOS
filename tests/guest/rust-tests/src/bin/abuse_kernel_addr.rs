//! `SYS_FUTEX_WAIT` takes an address from userland and hands it to
//! `AddressSpace::translate`, which walked any PML4 index. A user address space
//! shallow-copies the kernel's PML4 half, so a kernel address resolved to a
//! present, writable 2 MiB leaf of the direct map. The word was guarded, but by
//! an expression at the dispatch arm whose value was thrown away, in a
//! different file from the `pub fn` it protected. The bound now lives where it
//! cannot be forgotten — in `translate` itself, and in `futex_wait`'s
//! signature.
//!
//! **The verdict is not the assertion.** A kernel that still read there would
//! return the same error to a userland that cannot read a byte of the kernel's
//! address space to notice. So the kernel keeps sixteen bytes with a known
//! value and answers two questions about them (`SYS_DEBUG` actions 10 and 11):
//! where they are, and whether they still say what it put there.

use toyos_abi::syscall::{self, VmAt, VmProt, SyscallError};

use toyos_abi::syscall::debug_action::{CANARY_ADDR, CANARY_CHANGED};

const PAGE: u64 = syscall::PAGE_SIZE as u64;

fn err(ret: u64) -> Option<SyscallError> {
    SyscallError::from_u64(ret)
}

fn canary_intact() -> bool {
    syscall::debug(CANARY_CHANGED) == 0
}

fn main() {
    let canary = syscall::debug(CANARY_ADDR);
    assert!(
        err(canary).is_none() && canary >= 0xFFFF_8000_0000_0000,
        "SYS_DEBUG 10 did not answer with a kernel address ({canary:#x}); \
         this kernel has no canary and the test would pass vacuously",
    );
    assert!(canary_intact(), "the canary was already changed before the test ran");

    // SAFETY: placed anywhere, so it replaces nothing.
    let region = unsafe { syscall::vm_reserve(2 * PAGE as usize, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("reserve two pages");
    let base = region as u64;

    // The futex word, which the *scheduler* dereferences on every wake check
    // long after the syscall returned. A kernel address here is a read
    // oracle: `futex_wait` blocks when the word equals `expected` and returns
    // at once when it does not, so a caller that may name kernel memory can
    // ask whether a kernel word holds a value — one bit per call, from
    // timing alone. Neither answer is what an address it may not name gets.
    for &expected in &[0u32, 0x0F17_1E55, 0x1A55] {
        let ret = unsafe {
            syscall::futex_wait(canary as *const u32, expected, Some(50_000_000))
        };
        assert_eq!(
            err(ret),
            Some(SyscallError::BadAddress),
            "futex_wait answered {ret:#x} for a kernel address and expected={expected:#x}",
        );
    }
    let ret = unsafe { syscall::futex_wake(canary as *const u32, 1) };
    assert_eq!(err(ret), Some(SyscallError::BadAddress), "futex_wake took a kernel address");

    // The word must be one the scheduler can keep reading, so it is refused for
    // its alignment too — an unaligned one reads its tail out of the next
    // physical page.
    let word = base as *mut u32;
    unsafe { word.write_volatile(1) };
    let ret = unsafe { syscall::futex_wait((base + 2) as *const u32, 1, Some(0)) };
    assert_eq!(err(ret), Some(SyscallError::BadAddress), "futex_wait took an unaligned word");

    // And an honest futex still answers: the value does not match, so the call
    // returns rather than blocking.
    let ret = unsafe { syscall::futex_wait(word, 2, Some(50_000_000)) };
    assert!(err(ret).is_none(), "futex_wait refused a word of this process's own: {ret:#x}");
    let ret = unsafe { syscall::futex_wake(word, 1) };
    assert!(err(ret).is_none(), "futex_wake refused a word of this process's own: {ret:#x}");

    unsafe { syscall::vm_release(region, 2 * PAGE as usize) }.expect("release");
    assert!(canary_intact(), "the canary changed during the run");
    println!("futex refuses an address it may not reach, and kernel memory is untouched");
}
