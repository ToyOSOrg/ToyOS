//! Loom: the stop's claim on the console wire against a holder's letting go
//! (`kernel/src/log/handoff.rs`), over the real `SleepLock`.
//!
//! The holder, `klogd` here, lets the wire go and reads whether the stop
//! asked; the stop asks, and tries the wire. One of them must see the other:
//! the stop's try finds the wire free, or the holder sees the ask, and so
//! posts the release the stop parks on. Where neither does, the stop waits out its whole budget
//! on a wire nobody holds and writes over it. Removing either `SeqCst` fence
//! from `handoff.rs` reds this model with exactly that outcome.

#![cfg(feature = "loom")]

use kernel_loom::log_handoff::Handoff;
use kernel_loom::scheduler::{become_task, TaskId};
use kernel_loom::sleeplock::SleepLock;

const KLOGD: TaskId = TaskId(1, 0);
const STOP: TaskId = TaskId(2, 1);

#[test]
fn the_stop_finds_the_wire_free_or_klogd_sees_the_ask() {
    loom::model(|| {
        let wire: &'static SleepLock<()> = Box::leak(Box::new(SleepLock::new(())));
        let handoff: &'static Handoff = Box::leak(Box::new(Handoff::new()));

        become_task(KLOGD);
        let held = wire.try_lock().expect("nobody else has the wire yet");
        let klogd = loom::thread::spawn(move || {
            become_task(KLOGD);
            drop(held);
            handoff.asked()
        });

        become_task(STOP);
        handoff.ask();
        let took = wire.try_lock().is_some();
        let saw = klogd.join().unwrap();
        assert!(took || saw, "the stop's try found the wire held and klogd never saw the ask: nobody posts");
    });
}
