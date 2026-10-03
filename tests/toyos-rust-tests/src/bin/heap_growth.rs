//! **A burst of thread creations that grows the kernel heap, in a windows
//! report of its own.**
//!
//! A thread costs the kernel a 128 KiB stack off its heap, so [`THREADS`]
//! alive at once grow it by whole 2 MiB frames, each growth inside the
//! `SYS_THREAD_SPAWN` that needed it. A `mask-windows` kernel reports every
//! CPU's longest windows at each process exit, so two exits bracket the burst:
//! `echo`'s before it, which takes this process's spawn and its child's, and
//! the child's after it, the first exit recorded under this binary's name,
//! whose report `heap_growth_windows` reads. No thread ends inside the
//! bracket: an ending thread's TLS unmap is a shootdown, a window on every CPU.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const SELF_PATH: &str = "/system/bin/test_rs_heap_growth";

/// 32 MiB of kernel stacks.
const THREADS: usize = 256;

static RELEASED: AtomicBool = AtomicBool::new(false);

fn main() {
    if std::env::args().nth(1).as_deref() == Some("bracket") {
        let mut stdout = std::io::stdout();
        stdout.write_all(b"!").and_then(|()| stdout.flush()).expect("the bracket says it started");
        std::io::stdin().read_to_end(&mut Vec::new()).expect("the bracket waits for its stdin to close");
        return;
    }

    let mut bracket = Command::new(SELF_PATH)
        .arg("bracket")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the bracket");
    // Its start, too, is in `echo`'s report rather than the burst's.
    let mut started = [0u8];
    bracket.stdout.take().expect("the bracket's stdout").read_exact(&mut started).expect("the bracket started");
    let opened = Command::new("/system/bin/echo").status().expect("run echo");
    assert!(opened.success(), "echo: {opened}");

    let parked: Vec<_> = (0..THREADS)
        .map(|_| {
            thread::spawn(|| {
                while !RELEASED.load(Ordering::Acquire) {
                    thread::park();
                }
            })
        })
        .collect();

    drop(bracket.stdin.take());
    let closed = bracket.wait().expect("wait for the bracket");
    assert!(closed.success(), "the bracket: {closed}");

    RELEASED.store(true, Ordering::Release);
    for thread in &parked {
        thread.thread().unpark();
    }
    for thread in parked {
        thread.join().expect("a parked thread");
    }
    println!("heap_growth: {THREADS} threads alive at once between two process exits");
}
