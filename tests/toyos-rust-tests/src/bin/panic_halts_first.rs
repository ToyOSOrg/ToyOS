//! Threads that make kernel records as fast as they can while this one has
//! the kernel go fatal (`SYS_DEBUG` action 3). A sibling still running after
//! the fatal path began is a record stamped after the fatal one;
//! `panic_halts_the_others_first` reads the console for one.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A retired syscall's number: each call is refused and is one kernel record
/// naming it.
const RETIRED: u64 = 26;
/// One per other CPU of the boot that runs this.
const SIBLINGS: usize = 3;
/// A liveness guard on the siblings' first records.
const STARTED_WITHIN: Duration = Duration::from_secs(10);

fn retired() {
    let ret: u64;
    // SAFETY: a register-only `syscall` whose number the kernel refuses
    // without reading any argument; nothing in this process is touched.
    unsafe {
        core::arch::asm!("syscall", in("rdi") RETIRED, lateout("rax") ret, out("rcx") _, out("r11") _);
    }
    assert_ne!(ret, 0, "syscall {RETIRED} answered as if it were live");
}

fn main() {
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..SIBLINGS {
        let started = Arc::clone(&started);
        std::thread::spawn(move || {
            retired();
            started.fetch_add(1, Ordering::Release);
            loop {
                retired();
            }
        });
    }
    let until = Instant::now() + STARTED_WITHIN;
    while started.load(Ordering::Acquire) < SIBLINGS {
        assert!(Instant::now() < until, "the siblings made no record in {STARTED_WITHIN:?}");
        std::hint::spin_loop();
    }
    let rc = toyos_abi::syscall::debug(toyos_abi::syscall::debug_action::FATAL_HALT);
    eprintln!("ERROR: SYS_DEBUG FATAL_HALT returned {rc:#x}");
    std::process::exit(1);
}
