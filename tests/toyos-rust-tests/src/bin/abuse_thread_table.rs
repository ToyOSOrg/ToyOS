//! A process's threads are bounded at `MAX_THREADS`, exited ones not yet
//! joined among them: past it a spawn is refused by name, and a join is room
//! for one.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use toyos_abi::syscall::{self, MmapFlags, MmapProt, SyscallError, MAX_THREADS};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// One thread's stack. Each gets its own: a thread runs on after it says it
/// ran, so the next one never starts on a stack still in use.
const STACK: usize = 16 * 1024;

/// Spawns attempted: past where a thread table with no bound outgrows one
/// heap allocation.
const ATTEMPTS: usize = 4 * MAX_THREADS;

/// The hang ceiling on one thread getting to run.
const RUN_BOUND: Duration = Duration::from_secs(10);

/// Raw, so a thread costs its kernel record and nothing of std's.
extern "C" fn body(ran: u64) {
    // SAFETY: `ran` is the address of `main`'s flag, which outlives every thread.
    let ran = unsafe { &*(ran as *const AtomicU32) };
    ran.store(1, Ordering::Release);
    // SAFETY: an aligned `u32` the flag owns.
    unsafe { syscall::futex_wake(ran.as_ptr(), 1) };
    syscall::thread_exit(0);
}

/// Fresh stacks carved out of 2 MiB mappings.
struct Stacks {
    chunk: usize,
    used: usize,
}

impl Stacks {
    /// `(base, top)` of a stack no thread has run on.
    fn next(&mut self) -> (u64, u64) {
        if self.chunk == 0 || self.used == PAGE_2M {
            let chunk = unsafe {
                syscall::mmap(
                    core::ptr::null_mut(),
                    PAGE_2M,
                    MmapProt::READ | MmapProt::WRITE,
                    MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
                )
            };
            assert!(!chunk.is_null(), "no memory for thread stacks");
            self.chunk = chunk as usize;
            self.used = 0;
        }
        let base = (self.chunk + self.used) as u64;
        self.used += STACK;
        (base, base + STACK as u64)
    }
}

/// Start `body` on a fresh stack and wait for it to run; the refusal if the
/// kernel gave none.
fn spawn(stacks: &mut Stacks, ran: &AtomicU32) -> Result<u64, SyscallError> {
    let (base, top) = stacks.next();
    ran.store(0, Ordering::Relaxed);
    let tid = unsafe { syscall::thread_spawn(body as *const () as u64, top, ran as *const _ as u64, base) };
    if let Some(e) = SyscallError::from_u64(tid) {
        return Err(e);
    }
    let deadline = Instant::now() + RUN_BOUND;
    while ran.load(Ordering::Acquire) == 0 {
        let left = deadline.checked_duration_since(Instant::now()).unwrap_or_else(|| {
            panic!("thread {tid} did not run within {RUN_BOUND:?}");
        });
        unsafe { syscall::futex_wait(ran.as_ptr(), 0, Some(left.as_nanos() as u64)) };
    }
    Ok(tid)
}

fn main() {
    let ran = AtomicU32::new(0);
    let mut stacks = Stacks { chunk: 0, used: 0 };

    let mut first = None;
    let mut spawned = 0usize;
    let mut refusal = None;
    for _ in 0..ATTEMPTS {
        match spawn(&mut stacks, &ran) {
            Ok(tid) => {
                first.get_or_insert(tid);
                spawned += 1;
            }
            Err(e) => {
                refusal = Some(e);
                break;
            }
        }
    }
    let refusal = refusal.unwrap_or_else(|| {
        panic!("{spawned} threads exited unjoined and none was refused: the thread count is unbounded")
    });
    assert_eq!(
        refusal,
        SyscallError::ResourceExhausted,
        "the spawn after {spawned} was refused for another reason",
    );
    // This thread is the one more.
    assert_eq!(spawned, MAX_THREADS - 1, "refused after {spawned} exited threads beside this one");

    let first = first.expect("a thread was spawned");
    assert_eq!(syscall::thread_join(first), 0, "joining the first exited thread failed");
    spawn(&mut stacks, &ran).expect("a spawn found no room the join made");
    assert_eq!(
        spawn(&mut stacks, &ran),
        Err(SyscallError::ResourceExhausted),
        "a spawn past the bound was admitted",
    );

    println!("{spawned} threads exited unjoined and the next was refused; a join is room for one");
}
