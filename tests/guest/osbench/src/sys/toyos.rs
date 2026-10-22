use std::sync::atomic::AtomicU32;

use toyos_abi::syscall::{self, MmapFlags, MmapProt};

/// ToyOS has no call that says which CPU runs a thread, so CPUs are named as
/// the architecture's own instructions name them.
pub use crate::arch::{core_of, cpu_model, current_cpu, threads_per_core};

pub fn null_syscall() {
    std::hint::black_box(syscall::getpid());
}

/// Nanoseconds since boot off the clock page, which `Instant` reads and
/// every process shares.
pub fn monotonic_ns() -> u64 {
    toyos_abi::clock::nanos_since_boot()
}

/// Block while `word` holds `expected`.
pub fn futex_wait(word: &AtomicU32, expected: u32) {
    // SAFETY: `word` is a live, aligned u32.
    unsafe { syscall::futex_wait(word.as_ptr(), expected, None) };
}

/// Wake up to `count` waiters on `word`; answers how many woke.
pub fn futex_wake(word: &AtomicU32, count: u32) -> u32 {
    // SAFETY: `word` is a live, aligned u32.
    let woken = unsafe { syscall::futex_wake(word.as_ptr(), count) };
    u32::try_from(woken).expect("a wake count fits the count asked for")
}

/// ToyOS commits, zeroes and maps an anonymous range whole, in 2 MiB pages,
/// before `mmap` answers.
pub fn map_anon(len: usize) -> *mut u8 {
    // SAFETY: a fresh anonymous mapping aliases nothing.
    let ptr =
        unsafe { syscall::mmap(std::ptr::null_mut(), len, MmapProt::READ | MmapProt::WRITE, MmapFlags::ANONYMOUS) };
    assert!(!ptr.is_null(), "mmap of {len} bytes was refused");
    ptr
}

/// Every ToyOS anonymous mapping is in 2 MiB pages.
pub fn map_anon_huge(len: usize) -> *mut u8 {
    map_anon(len)
}

pub fn huge_backing() -> String {
    "all".into()
}

/// # Safety
/// `ptr` and `len` are one whole mapping from [`map_anon`], and nothing
/// reaches it afterwards.
pub unsafe fn unmap(ptr: *mut u8, len: usize) {
    // SAFETY: as the caller promises.
    unsafe { syscall::munmap(ptr, len) }.expect("munmap of a whole mapping");
}

/// Nothing a program can read: the kernel's frequency request and the
/// machine's power source are the boot log's and the operator's to record.
pub fn conditions() {}
