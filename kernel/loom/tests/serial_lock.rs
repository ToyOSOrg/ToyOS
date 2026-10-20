//! Loom: a lost `try_lock` on the console backend leaves its holder holding;
//! a fatal path's `try_seize` takes a hold another CPU let go, is refused one
//! it keeps, and has its own CPU's hold at once; and a burst beneath its own
//! CPU's fatal path is refused rather than spun on.
//!
//! A loss that released the lock let a third writer in under the holder. The
//! negative control is the `serial-try-lock-then-some` feature, which must red
//! this file.
#![cfg(feature = "loom")]

use kernel_loom::arch::cpu::become_cpu;
use kernel_loom::serial_lock::{BackendLock, Seized};
use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::thread;

/// One CPU: a loss while it holds the lock is a loss again on the next try.
#[test]
fn a_lost_try_lock_leaves_the_lock_held() {
    loom::model(|| {
        let lock = BackendLock::new();
        let held = lock.try_lock().expect("an unheld lock refused its first taker");
        assert!(lock.try_lock().is_none(), "a held lock was taken twice");
        assert!(lock.try_lock().is_none(), "a lost try_lock released the lock its holder still has");
        drop(held);
        assert!(lock.try_lock().is_some(), "a released lock stayed held");
    });
}

/// Two CPUs, each trying twice: whoever gets in writes alone.
#[test]
fn two_writers_never_overlap() {
    loom::model(|| {
        let lock = Arc::new(BackendLock::new());
        let line = Arc::new(UnsafeCell::new(0u32));
        let writer = |lock: Arc<BackendLock>, line: Arc<UnsafeCell<u32>>| {
            move || {
                for _ in 0..2 {
                    if let Some(held) = lock.try_lock() {
                        // SAFETY: the backend is held, so no other writer is in here.
                        line.with_mut(|n| unsafe { *n += 1 });
                        drop(held);
                        return;
                    }
                    thread::yield_now();
                }
            }
        };
        let other = thread::spawn(writer(lock.clone(), line.clone()));
        writer(lock, line)();
        other.join().unwrap();
    });
}

loom::lazy_static! {
    /// A lock whose burst can be handed to another thread: [`Held`] borrows it.
    static ref LOCK: BackendLock = BackendLock::new();
}

/// A burst held when a fatal path starts asking, and let go on another CPU, is
/// taken. Each ask before the holder has let go, which loom counts as a
/// preemption of the holder, costs one, so `k` of them leave `k + 2` asks
/// enough; the kernel's wait is bounded by time, which loom has none of.
#[test]
fn a_seize_takes_a_burst_let_go_on_another_cpu() {
    for preemptions in 0..=2u8 {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(usize::from(preemptions));
        model.check(move || {
            become_cpu(0);
            let burst = LOCK.try_lock().expect("an unheld lock refused its first taker");
            let holder = thread::spawn(move || drop(burst));
            let taken = (0..u64::from(preemptions) + 2).any(|_| {
                let taken = matches!(LOCK.try_seize(), Some(Seized::Taken(_)));
                loom::hint::spin_loop();
                taken
            });
            // Joined first: a holder never run still owns the burst, whose
            // drop outside the model would abort the binary rather than fail.
            holder.join().unwrap();
            assert!(taken, "a fatal path was refused a burst that let go");
        });
    }
}

/// A holder that keeps the lock has every ask refused: the fatal path's
/// bounded wait is all that ends it.
#[test]
fn a_seize_is_refused_a_holder_that_keeps_the_lock() {
    loom::model(|| {
        become_cpu(0);
        let lock = BackendLock::new();
        let _burst = lock.try_lock().expect("an unheld lock refused its first taker");
        assert!(lock.try_seize().is_none(), "a seize took a lock its holder kept");
    });
}

/// A fatal path entered on top of its own CPU's has the lock at once, and
/// another CPU's still waits the holder out.
#[test]
fn a_seize_reenters_its_own_cpus_hold() {
    loom::model(|| {
        let lock = Arc::new(BackendLock::new());
        become_cpu(1);
        let Some(Seized::Taken(held)) = lock.try_seize() else { panic!("a free lock refused a seize") };
        assert!(matches!(lock.try_seize(), Some(Seized::Reentered)), "a fatal path was refused its own CPU's hold");
        assert!(lock.try_lock().is_none(), "a burst took a fatal path's hold");
        let other = {
            let lock = lock.clone();
            thread::spawn(move || {
                become_cpu(2);
                lock.try_seize().is_none()
            })
        };
        assert!(other.join().unwrap(), "a fatal path took another CPU's hold");
        drop(held);
        assert!(matches!(lock.try_seize(), Some(Seized::Taken(_))), "a released lock stayed held");
    });
}

/// A fatal path and a burst on two CPUs: whoever gets in writes alone.
#[test]
fn a_seize_and_a_burst_never_overlap() {
    loom::model(|| {
        let lock = Arc::new(BackendLock::new());
        let line = Arc::new(UnsafeCell::new(0u32));
        let burst = {
            let (lock, line) = (lock.clone(), line.clone());
            thread::spawn(move || {
                if let Some(held) = lock.try_lock() {
                    // SAFETY: the backend is held, so no other writer is in here.
                    line.with_mut(|n| unsafe { *n += 1 });
                    drop(held);
                }
            })
        };
        become_cpu(0);
        if let Some(Seized::Taken(held)) = lock.try_seize() {
            // SAFETY: as above.
            line.with_mut(|n| unsafe { *n += 1 });
            drop(held);
        }
        burst.join().unwrap();
    });
}

/// A burst on the CPU whose fatal path holds the registers would wait for a
/// frame beneath it: it is refused, loudly.
#[test]
#[should_panic(expected = "serial: a burst asked for the console registers this cpu's own fatal path holds")]
fn a_burst_beneath_its_own_cpus_fatal_path_is_refused() {
    loom::model(|| {
        become_cpu(1);
        let lock = BackendLock::new();
        let _fatal = lock.try_seize();
        drop(lock.lock());
    });
}

/// A burst on another CPU waits a fatal path's hold out, and has the lock
/// once it is let go.
#[test]
fn a_burst_waits_out_another_cpus_fatal_path() {
    loom::model(|| {
        let lock = Arc::new(BackendLock::new());
        become_cpu(1);
        let Some(Seized::Taken(fatal)) = lock.try_seize() else { panic!("a free lock refused a seize") };
        let burst = {
            let lock = lock.clone();
            thread::spawn(move || {
                become_cpu(2);
                drop(lock.lock());
            })
        };
        drop(fatal);
        burst.join().unwrap();
    });
}
