//! Loom: the interrupt record a process's PCI driver reads.
//!
//! The kernel programs one MSI-X vector per claimed function and accumulates
//! what arrives into a record its holder reads through a syscall. So there are
//! two parties on two CPUs and one word between them: an ISR that bumps a
//! count, and a reader that takes it.
//!
//! **The invariant is that every message is counted exactly once.** Nothing
//! here orders anything else — the word is the whole of what it says, so the
//! orderings are `Relaxed` and the property is an interleaving. What makes it
//! hold is that the taking side is a read-modify-write: a reader that loaded a
//! count and then cleared it drops every message the ISR recorded in between.
//!
//! That is the record's whole design, so the negative control is it turned off
//! — a cargo feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features device-irq-lossy \
//!   --test device_irq
//! ```
//!
//! makes the `swap` a load and a store and the ISR's `fetch_add` a load, an
//! add and a store, and [`every_message_is_counted_once`] must red.

use kernel_loom::device_irq::Interrupt;
use loom::sync::Arc;

/// No message is lost, and none is counted twice.
///
/// Two messages against one reader that may run at any point between them: what
/// the reader took plus what is left is exactly what arrived. A `store` where
/// the count has a `fetch_add` fails this, and so does a reader that loads and
/// then clears instead of swapping.
#[test]
fn every_message_is_counted_once() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());

        let isr = {
            let irq = irq.clone();
            loom::thread::spawn(move || {
                irq.took();
                irq.took();
            })
        };

        let taken = irq.take().unwrap_or(0);
        isr.join().unwrap();
        let left = irq.take().unwrap_or(0);

        assert_eq!(
            taken + left,
            2,
            "two interrupts arrived, the reader took {taken} and {left} were left: a driver \
             that misses one waits for a device that has already spoken",
        );
    });
}

/// A reader that finds nothing answers nothing, and leaves nothing behind.
///
/// Single-threaded and deliberately so: this is the answer on the path a
/// poller takes every time round its loop on a quiet link, and it must not
/// depend on what any other CPU is doing.
#[test]
fn an_idle_record_answers_nothing() {
    loom::model(|| {
        let irq = Interrupt::new();
        assert!(irq.take().is_none(), "an untouched record answered a reader");
        assert!(!irq.armed(), "an untouched record read ready");
        irq.took();
        assert!(irq.armed(), "a record with a message in it read empty");
        assert_eq!(irq.take(), Some(1));
        assert!(irq.take().is_none(), "the same message was answered twice");
        assert!(!irq.armed(), "a drained record still reads ready");
    });
}
