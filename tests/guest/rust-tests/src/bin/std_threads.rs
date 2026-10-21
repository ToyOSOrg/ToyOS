//! What std's threads take from the kernel and give back: the CPU count, and
//! the stack and table place of a thread once it has ended, joined or not.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use toyos_abi::syscall::MAX_THREADS;

/// Threads spawned and joined one after another.
const JOINS: usize = 64;

/// The base of the stack the calling thread runs on.
fn stack_base() -> u64 {
    toyos_abi::syscall::stack_info().expect("a spawned thread has a stack").0
}

fn main() {
    // The harness holds the printed count to the CPUs it booted; on a bench,
    // the kernel's own count is the only one at hand.
    let cpus = thread::available_parallelism().expect("available_parallelism").get();
    let counted = toyos_abi::syscall::cpu_count() as usize;
    assert_eq!(cpus, counted, "available_parallelism answers {cpus}, and the kernel counts {counted} CPUs");
    println!("std_threads: {cpus} CPUs");

    // Stacks still allocated cannot share a base. A freed one can be the next
    // spawn's, when the allocator gives the mapping back and the kernel places
    // the next at the same address.
    let joined: BTreeSet<u64> =
        (0..JOINS).map(|_| thread::spawn(stack_base).join().expect("an empty thread")).collect();
    assert!(
        joined.len() < JOINS,
        "{JOINS} threads spawned and joined one at a time had {JOINS} distinct stacks: no joined thread's \
         stack came back"
    );

    // More detached threads than a process may hold, each ended before the
    // next spawn: every one is refused nothing only if the spawns collect the
    // ones before them, and their stacks share bases only if they are freed.
    let detached = MAX_THREADS + 8;
    static ENDED: AtomicUsize = AtomicUsize::new(0);
    static BASES: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
    for spawned in 0..detached {
        let handle = thread::Builder::new()
            .spawn(|| {
                BASES.lock().unwrap().push(stack_base());
                ENDED.fetch_add(1, Ordering::Release);
            })
            .unwrap_or_else(|e| panic!("detached thread {spawned} of {detached} was refused: {e}"));
        drop(handle);
        while ENDED.load(Ordering::Acquire) <= spawned {
            std::hint::spin_loop();
        }
    }
    let bases: BTreeSet<u64> = BASES.lock().unwrap().iter().copied().collect();
    assert!(
        bases.len() < detached,
        "{detached} detached threads, each ended before the next spawn, had {detached} distinct stacks: no \
         detached thread's stack came back"
    );
    println!(
        "std_threads: {JOINS} joined threads ran on {} stacks; {detached} detached threads were all \
         spawned and ran on {} stacks",
        joined.len(),
        bases.len()
    );
}
