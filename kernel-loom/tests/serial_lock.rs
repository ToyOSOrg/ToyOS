//! Loom: a lost `try_lock` on the console backend leaves its holder holding.
//!
//! The console drain takes the backend with `try_lock` from every CPU that
//! logs, so a losing attempt is the common case beside another CPU's write.
//! A loss that released the lock let a third writer in under the holder. The
//! negative control is the `serial-try-lock-then-some` feature, which must red
//! this file.
#![cfg(feature = "loom")]

use kernel_loom::serial_lock::BackendLock;
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
        writer(lock.clone(), line.clone())();
        other.join().unwrap();
    });
}
