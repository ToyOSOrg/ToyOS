//! A typed copy into user memory must never land in a frame a sibling has
//! been handed since the copy translated its destination.
//!
//! The kernel, armed with `copy-meets-a-remap`, holds a copy whose destination
//! carries the mark below between its translation and its store, writes the
//! cue into the destination's second word, and lets the store go once this
//! process has mapped memory again. The main thread waits for the cue, unmaps
//! the destination and maps a fresh region, which the physical allocator
//! serves from the lowest free frame: the one just unmapped, unless the copy
//! holds it. A kernel that holds nothing across the copy stores into that
//! region; one that pins the frame leaves it as the allocator zeroed it.
//!
//! **The memory is the verdict, before the return value.**

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use toyos_abi::syscall::{self, MmapFlags, MmapProt, OpenFlags, SYS_FSTAT};

/// `kernel/src/user_ptr.rs`'s `remap_race::MARK` and `HELD`.
const MARK: u64 = 0x5eed_c0de_2ace_0001;
const HELD: u64 = 0x5eed_c0de_2ace_0002;

const SELF_PATH: &str = "/system/bin/test_rs_copy_out_races_munmap";
const PAGE_2M: usize = 2 * 1024 * 1024;
/// Past `Stat`, which is what `fstat` stores.
const CHECKED: usize = 64;

/// `fstat` into an address, which the typed wrapper cannot take.
fn fstat_into(handle: u64, addr: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") SYS_FSTAT,
            in("rsi") handle,
            in("rdx") addr,
            in("r8") 0u64,
            in("r9") 0u64,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

fn map_2m() -> *mut u8 {
    let p = unsafe {
        syscall::mmap(
            core::ptr::null_mut(),
            PAGE_2M,
            MmapProt::READ | MmapProt::WRITE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!p.is_null(), "mmap of a 2 MiB region failed");
    p
}

fn main() {
    let fd = syscall::open(SELF_PATH.as_bytes(), OpenFlags::READ).expect("open self");
    let victim = map_2m();
    let words = victim.cast::<u64>();
    unsafe {
        words.write_volatile(MARK);
        words.add(1).write_volatile(0);
    }

    let ret = Arc::new(AtomicU64::new(u64::MAX));
    let copier = {
        let ret = Arc::clone(&ret);
        let (fd, addr) = (fd.0 as u64, victim as u64);
        thread::spawn(move || ret.store(fstat_into(fd, addr), Ordering::SeqCst))
    };

    // No deadline: a kernel that never holds the marked copy leaves this spinning,
    // and the harness ceiling reds it.
    while unsafe { words.add(1).read_volatile() } != HELD {
        std::hint::spin_loop();
    }
    unsafe { syscall::munmap(victim, PAGE_2M) }.expect("munmap the copy's destination");
    let sibling = map_2m();

    copier.join().expect("the copying thread panicked");
    let got: Vec<u8> = (0..CHECKED).map(|i| unsafe { sibling.add(i).read_volatile() }).collect();
    if let Some(at) = got.iter().position(|&b| b != 0) {
        panic!(
            "a copy held across a sibling's munmap stored into the region mapped after it: byte \
             {at} is {:#x}, and the region was zeroed when it was handed out: {got:x?}",
            got[at]
        );
    }
    assert_eq!(ret.load(Ordering::SeqCst), 0, "the held fstat did not succeed");
    syscall::close(fd);
    println!("copy_out_races_munmap: the copy held its frame across the sibling's munmap and mmap");
}
