//! Loom: a lost `try_lock` on the console backend leaves its holder holding,
//! and a fatal path's `seize` gives up on a holder that never lets go but not
//! on its own CPU's.
//!
//! The console drain takes the backend with `try_lock` from every CPU that
//! logs, so a losing attempt is the common case beside another CPU's write.
//! A loss that released the lock let a third writer in under the holder. The
//! negative control is the `serial-try-lock-then-some` feature, which must red
//! this file.
#![cfg(feature = "loom")]

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

/// Nothing on a fatal path waits for ever, and a holder that never lets go is
/// what it would wait for.
#[test]
fn a_seize_gives_up_on_a_holder_that_never_lets_go() {
    loom::model(|| {
        let lock = BackendLock::new();
        let _burst = lock.try_lock().expect("an unheld lock refused its first taker");
        assert!(matches!(lock.seize(0, 3), Seized::Expired), "a seize took a lock its holder kept");
    });
}

/// A fatal path entered on top of its own CPU's has the lock at once, and
/// another CPU's still waits the holder out.
#[test]
fn a_seize_reenters_its_own_cpus_hold() {
    loom::model(|| {
        let lock = BackendLock::new();
        let Seized::Taken(held) = lock.seize(1, 1) else { panic!("a free lock refused a seize") };
        assert!(matches!(lock.seize(1, 1), Seized::Reentered), "a fatal path waited out its own CPU's hold");
        assert!(matches!(lock.seize(2, 1), Seized::Expired), "a fatal path took another CPU's hold");
        assert!(lock.try_lock().is_none(), "a burst took a fatal path's hold");
        drop(held);
        assert!(matches!(lock.seize(2, 1), Seized::Taken(_)), "a released lock stayed held");
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
        if let Seized::Taken(held) = lock.seize(0, 2) {
            // SAFETY: as above.
            line.with_mut(|n| unsafe { *n += 1 });
            drop(held);
        }
        burst.join().unwrap();
    });
}
