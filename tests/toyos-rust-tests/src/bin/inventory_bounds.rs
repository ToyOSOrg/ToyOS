//! `SYS_DEVICE_INVENTORY`'s two refusals, each at its edge.
//!
//! A buffer one record short is refused whole and nothing is written into it;
//! a declared count past the bound is refused before it becomes a window, and
//! so is one whose length in bytes wraps. The count the empty buffer answers is
//! the premise of all three, so it is asserted first.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::inventory::{RawRecord, RECORD_BYTES};
use toyos_abi::syscall::{SyscallError, SYS_DEVICE_INVENTORY};

/// The kernel's bound on a declared count.
const MAX_RECORDS: usize = 1024;

/// `SYS_DEVICE_INVENTORY` with a count no slice can spell.
fn raw(cap: u64, buf: u64, count: u64) -> u64 {
    let ret: u64;
    // SAFETY: the kernel writes at most `count` records at `buf`, and every
    // call here either points `buf` at that many or expects a refusal before
    // anything is written.
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") SYS_DEVICE_INVENTORY,
            in("rsi") cap,
            in("rdx") buf,
            in("r8") count,
            in("r9") 0u64,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");

    let count = cap.inventory(&mut []).expect("an empty buffer asks how many");
    assert!(count > 1, "the machine has {count} records, too few to be one short of");
    assert!(count <= MAX_RECORDS, "the machine has {count} records, past the bound");
    println!("inventory bounds: an empty buffer answers {count}");

    let mut short = vec![RawRecord::EMPTY; count - 1];
    assert_eq!(cap.inventory(&mut short), Err(SyscallError::ResourceExhausted));
    assert!(short.iter().all(|r| *r == RawRecord::EMPTY), "a refused call wrote a record");
    println!("inventory bounds: {} records is refused whole", count - 1);

    let mut whole = vec![RawRecord::EMPTY; count];
    assert_eq!(cap.inventory(&mut whole), Ok(count), "the premise: the machine did not change");

    let handle = u64::from(cap.as_handle().0);
    let mut past = vec![RawRecord::EMPTY; MAX_RECORDS + 1];
    let answer = raw(handle, past.as_mut_ptr() as u64, past.len() as u64);
    assert_eq!(SyscallError::from_u64(answer), Some(SyscallError::InvalidArgument), "{answer:#x}");
    assert!(past.iter().all(|r| *r == RawRecord::EMPTY));
    println!("inventory bounds: {} records is refused", MAX_RECORDS + 1);

    // Times the record width, this is 2^64 + 64: one record's worth once it
    // wraps, and a buffer that really is one record long.
    let wraps = (1u64 << 58) + 1;
    assert_eq!(wraps.wrapping_mul(RECORD_BYTES as u64), RECORD_BYTES as u64);
    let mut one = [RawRecord::EMPTY];
    let answer = raw(handle, one.as_mut_ptr() as u64, wraps);
    assert_eq!(SyscallError::from_u64(answer), Some(SyscallError::InvalidArgument), "{answer:#x}");
    assert_eq!(one[0], RawRecord::EMPTY);
    println!("inventory bounds: a count whose length wraps is refused");
}
