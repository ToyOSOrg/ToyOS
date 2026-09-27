//! Threads of one process sharing one directory's connection are served in turn.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

const THREADS: usize = 6;
/// The passes the first thread to finish makes.
const PASSES: u32 = 32;

fn main() {
    std::fs::create_dir_all("/home/fs_turns").expect("make /home/fs_turns");
    let start = Arc::new(Barrier::new(THREADS));
    let done = Arc::new(AtomicBool::new(false));
    let workers: Vec<_> = (0..THREADS)
        .map(|n| {
            let (start, done) = (Arc::clone(&start), Arc::clone(&done));
            std::thread::spawn(move || {
                let path = format!("/home/fs_turns/{n}");
                start.wait();
                let mut passes: u32 = 0;
                while !done.load(Ordering::Acquire) {
                    std::fs::write(&path, passes.to_le_bytes()).unwrap_or_else(|e| panic!("thread {n}: write {path}: {e}"));
                    passes += 1;
                    if passes == PASSES {
                        done.store(true, Ordering::Release);
                    }
                }
                passes
            })
        })
        .collect();
    let passes: Vec<u32> = workers.into_iter().map(|w| w.join().expect("a writer panicked")).collect();
    println!("fs_turns: passes per thread {passes:?}");
    let _ = std::fs::remove_dir_all("/home/fs_turns");
    let least = *passes.iter().min().expect("threads");
    // One pass is a lock that lets its releaser take it straight back: every
    // other thread gets in once, at the start. A floor any higher also
    // measures how long a thread is kept off-CPU between its reply and its
    // next request, when it holds no ticket.
    assert!(least > 1, "a thread made {least} passes while another made {PASSES}: {passes:?}");
    println!("fs_turns: PASS");
}
