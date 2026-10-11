//! Threads that make kernel records as fast as they can while this one has
//! the kernel go fatal (`SYS_DEBUG` action 3). A sibling still running after
//! the fatal path stopped the other CPUs is a record past the fatal path's own
//! line after the stop; `virt_fatal_halts_the_others_first` reads the console
//! for one.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use toyos_abi::syscall::debug_action::{FATAL_HALT, LOG_PATTERNED};

/// One per other CPU of the boot that runs this.
const SIBLINGS: usize = 3;

/// One kernel record, `logstorm t=0 i=0 …`.
fn record() {
    let answer = toyos_abi::syscall::debug_with(LOG_PATTERNED, 0);
    assert_eq!(answer, 0, "SYS_DEBUG LOG_PATTERNED answered {answer:#x}");
}

fn main() {
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..SIBLINGS {
        let started = Arc::clone(&started);
        std::thread::spawn(move || {
            record();
            started.fetch_add(1, Ordering::Release);
            loop {
                record();
            }
        });
    }
    // No deadline: siblings that never make a record are a hang the harness
    // ceiling reds.
    while started.load(Ordering::Acquire) < SIBLINGS {
        std::hint::spin_loop();
    }
    let rc = toyos_abi::syscall::debug(FATAL_HALT);
    eprintln!("ERROR: SYS_DEBUG FATAL_HALT returned {rc:#x}");
    std::process::exit(1);
}
