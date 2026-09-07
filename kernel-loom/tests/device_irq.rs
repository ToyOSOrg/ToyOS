//! Loom: the interrupt record a process's PCI driver reads.
//!
//! The kernel programs one MSI-X vector per claimed function and accumulates
//! what arrives into a record its holder reads through a syscall. So there are
//! two parties on two CPUs and two words between them: an ISR that bumps a
//! count and arms a wake, a reader that takes the count, and a scheduler pass
//! that takes the wake.
//!
//! **The invariant is that every message is counted exactly once and owes
//! exactly one wake.** Nothing here orders anything else — each word is the
//! whole of what it says, so the orderings are `Relaxed` and every property is
//! an interleaving. What makes them hold is that the taking side of both words
//! is a read-modify-write: a reader that loaded a count and then cleared it
//! drops every message the ISR recorded in between, and two scheduler passes
//! that both loaded a wake flag both wake one message's watchers.
//!
//! That pair is the record's whole design, so the negative control is the pair
//! turned off — a cargo feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features device-irq-lossy \
//!   --test device_irq
//! ```
//!
//! makes both `swap`s a load and a store and the ISR's `fetch_add` a load, an
//! add and a store, and this file must red — at
//! [`every_message_is_counted_once`] and at [`one_message_is_one_wake`], which
//! are the two defects stated exactly.

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

/// A wake is owed exactly once per message, and the pass that owes it is the
/// one that takes it.
///
/// The wake and the ISR are the *same* CPU in the kernel — the scheduler pass
/// that drains runs after the handler that armed it, with interrupts off in
/// between — so this models the weaker thing that must also hold: two passes
/// racing each other never both wake, and never both decline.
#[test]
fn one_message_is_one_wake() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());
        irq.took();

        let other = {
            let irq = irq.clone();
            loom::thread::spawn(move || irq.take_pending())
        };

        let mine = irq.take_pending();
        let theirs = other.join().unwrap();

        assert!(!(mine && theirs), "two passes both woke one message's watchers");
        assert!(mine || theirs, "neither pass woke a message that had already arrived");
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
