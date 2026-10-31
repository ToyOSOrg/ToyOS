//! What std's threads take from the kernel and give back: the CPU count and
//! the mask it is counted from, which a spawned thread and process inherit, and the
//! stack and table place of a thread once it has ended, joined or not.

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
    if std::env::args().nth(1).as_deref() == Some("mask") {
        let mask = toyos_abi::syscall::thread_affinity(own()).expect("its own mask");
        println!("{mask:?}");
        return;
    }
    // The harness holds the printed count to the CPUs it booted; on a bench,
    // the kernel's own count is the only one at hand.
    let cpus = thread::available_parallelism().expect("available_parallelism").get();
    let counted = toyos_abi::syscall::cpu_count() as usize;
    assert_eq!(cpus, counted, "available_parallelism answers {cpus}, and the kernel counts {counted} CPUs");
    println!("std_threads: {cpus} CPUs");

    // A narrowed mask is what std counts, and what a thread spawned under it
    // starts with; a mask wider again is accepted, the mask being a choice.
    let last = (counted - 1) as u64;
    let mut one = [0u64; toyos_abi::syscall::CPU_MASK_WORDS];
    one[(last / 64) as usize] = 1 << (last % 64);
    toyos_abi::syscall::thread_set_affinity(own(), &one).expect("a mask of one online CPU");
    assert_eq!(thread::available_parallelism().expect("available_parallelism").get(), 1);
    let (inherited, ran_on) = thread::spawn(|| {
        (toyos_abi::syscall::thread_affinity(own()).expect("its own mask"), toyos_abi::syscall::current_cpu())
    })
    .join()
    .expect("a thread reading its mask");
    assert_eq!(inherited, one, "a spawned thread starts with its spawner's mask");
    assert_eq!(u64::from(ran_on), last, "and runs where that mask names");
    let exe = std::env::current_exe().expect("this program's path");
    let child = std::process::Command::new(exe).arg("mask").output().expect("this program, asked for its mask");
    assert!(child.status.success(), "the child asked for its mask ended {:?}", child.status);
    assert_eq!(
        String::from_utf8_lossy(&child.stdout).trim(),
        format!("{one:?}"),
        "a spawned process starts with its spawning thread's mask"
    );
    let mut all = [0u64; toyos_abi::syscall::CPU_MASK_WORDS];
    for cpu in 0..counted {
        all[cpu / 64] |= 1 << (cpu % 64);
    }
    toyos_abi::syscall::thread_set_affinity(own(), &all).expect("every online CPU");
    assert_eq!(thread::available_parallelism().expect("available_parallelism").get(), counted);

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

/// The calling thread's id, as the affinity calls name it.
fn own() -> u64 {
    u64::from(toyos_abi::current_tid().0)
}
