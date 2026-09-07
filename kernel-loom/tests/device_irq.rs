//! Loom: the interrupt record a process's PCI driver reads.
//!
//! The kernel programs one MSI-X vector per claimed function and accumulates
//! what arrives into a record its holder reads through a syscall. So there are
//! two parties on two CPUs and one word between them: an ISR that stamps a
//! timestamp and bumps a count, and a reader that takes the count and reports
//! the timestamp with it.
//!
//! **The invariant is that a count carries a timestamp.** A record answering
//! "two interrupts, at nanosecond zero" is a driver told its device spoke
//! before the machine started; the only thing standing between the two fields
//! is the count's release edge carrying the timestamp store that precedes it.
//!
//! On x86 every store is a release and every load an acquire, so a build with
//! that edge relaxed behaves identically to this one and no guest test in the
//! suite can fail on it. The negative case is a cargo feature rather than a
//! comment:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features device-irq-relaxed \
//!   --test device_irq
//! ```
//!
//! makes `record.rs`'s publication and observation `Relaxed` and this file must
//! red, at [`a_count_always_carries_a_timestamp`] — *a record claimed 1
//! interrupt and timed it at nanosecond 0*, which is the defect stated exactly.

use kernel_loom::device_irq::Interrupt;
use loom::sync::Arc;

/// A plausible `nanos_since_boot`, and never zero: zero is the value the record
/// holds when nothing has arrived, which is what makes it the tell.
const TOOK_AT: u64 = 7;

/// A reader that sees a count sees the timestamp that went with it.
///
/// The model that reds with the edge relaxed. Both threads run once, because
/// one message is enough: the defect is an ordering, not an accumulation.
#[test]
fn a_count_always_carries_a_timestamp() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());

        let isr = {
            let irq = irq.clone();
            loom::thread::spawn(move || irq.took(TOOK_AT))
        };

        if let Some((count, at)) = irq.take() {
            assert_eq!(
                at, TOOK_AT,
                "a record claimed {count} interrupt(s) and timed them at nanosecond {at} — the \
                 count reached its reader without the stamp that belongs to it",
            );
        }
        isr.join().unwrap();
    });
}

/// No message is lost, and none is counted twice.
///
/// Two messages against one reader that may run at any point between them: what
/// the reader took plus what is left is exactly what arrived. A `store` where
/// the count has a `fetch_add` would fail this, and so would a reader that
/// loaded and then cleared instead of swapping.
#[test]
fn every_message_is_counted_once() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());

        let isr = {
            let irq = irq.clone();
            loom::thread::spawn(move || {
                irq.took(TOOK_AT);
                irq.took(TOOK_AT);
            })
        };

        let taken = irq.take().map_or(0, |(count, _)| count);
        isr.join().unwrap();
        let left = irq.take().map_or(0, |(count, _)| count);

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
        irq.took(TOOK_AT);

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
        irq.took(TOOK_AT);
        assert!(irq.armed(), "a record with a message in it read empty");
        assert_eq!(irq.take(), Some((1, TOOK_AT)));
        assert!(irq.take().is_none(), "the same message was answered twice");
        assert!(!irq.armed(), "a drained record still reads ready");
    });
}

/// A fault reaches the holder's next call.
///
/// The unit's fault handler stores this with no lock at all — it is an ISR —
/// and every call the claim answers reads it. Nothing else in the record
/// orders the two, so this is the whole edge.
#[test]
fn a_fault_reaches_the_next_call() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());

        let unit = {
            let irq = irq.clone();
            loom::thread::spawn(move || irq.fault())
        };

        // Whatever this reads is legal — the fault may not have happened yet.
        let _ = irq.faulted();
        unit.join().unwrap();
        assert!(irq.faulted(), "a fault the unit recorded never reached the claim that owns it");
    });
}
