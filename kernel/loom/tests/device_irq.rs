//! Loom: the interrupt record a process's PCI driver reads.
//!
//! The kernel programs one MSI-X vector per claimed function and accumulates
//! what arrives into a record its holder reads through a syscall. So there are
//! two parties on two CPUs: an ISR that counts a message and stamps when it
//! landed, and a reader that takes the count and its times.
//!
//! **The invariant is that every message is counted exactly once, and a
//! read's times are its own messages'.** What makes the first hold is that
//! both sides change the count by a read-modify-write: a reader that loaded a
//! count and then cleared it drops every message the ISR recorded in between.
//! That is the record's whole design, so the negative control is it turned off
//! — a cargo feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features device-irq-lossy \
//!   --test device_irq
//! ```
//!
//! makes every compare-exchange a load and a store, and
//! [`every_message_is_counted_once`] must red.

use kernel_loom::device_irq::Interrupt;
use loom::sync::Arc;

/// No message is lost, and none is counted twice.
///
/// Two messages against one reader that may run at any point between them: what
/// the reader took plus what is left is exactly what arrived. A `store` where
/// the count has a compare-exchange fails this, and so does a reader that
/// loads and then clears instead of exchanging.
#[test]
fn every_message_is_counted_once() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());

        let isr = {
            let irq = irq.clone();
            loom::thread::spawn(move || {
                irq.took(1);
                irq.took(2);
            })
        };

        let taken = irq.take().map_or(0, |record| record.count);
        isr.join().unwrap();
        let left = irq.take().map_or(0, |record| record.count);

        assert_eq!(
            taken + left,
            2,
            "two interrupts arrived, the reader took {taken} and {left} were left: a driver \
             that misses one waits for a device that has already spoken",
        );
    });
}

/// A read's first time is the landing of the first message it counts, and its
/// newest time no older than the newest it counts.
///
/// Three messages, landing at 1, 2 and 3, against a reader that takes once at
/// any point among them and then takes what is left: whatever the split, each
/// read's `first_nanos` is its own first message's, and a message that lands
/// while the reader is between its exchange and its loads stamps the next
/// read and never this one. A single slot for both reads' first times fails
/// this, and so does a stamp made after the exchange that counts it.
#[test]
fn a_reads_times_are_its_own_messages() {
    loom::model(|| {
        let irq = Arc::new(Interrupt::new());

        let isr = {
            let irq = irq.clone();
            loom::thread::spawn(move || {
                for at in 1..=3 {
                    irq.took(at);
                }
            })
        };

        let taken = irq.take();
        isr.join().unwrap();
        let left = irq.take();

        let counted = taken.map_or(0, |record| record.count);
        if let Some(record) = taken {
            assert_eq!(record.first_nanos, 1, "the first read counted from message 1 and was told {}", record.first_nanos);
            assert!(
                record.last_nanos >= u64::from(counted),
                "a read that counted {counted} message(s) was told the newest landed at {}",
                record.last_nanos
            );
        }
        match left {
            None => assert_eq!(counted, 3, "the first read counted {counted} and nothing was left"),
            Some(left) => {
                assert_eq!(
                    left.first_nanos,
                    u64::from(counted) + 1,
                    "the second read counted from message {} and was told {}",
                    counted + 1,
                    left.first_nanos
                );
                assert_eq!(left.last_nanos, 3, "the newest message landed at 3 and the last read was told {}", left.last_nanos);
            }
        }
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
        irq.took(7);
        assert!(irq.armed(), "a record with a message in it read empty");
        let record = irq.take().expect("a record with a message in it answered nothing");
        assert_eq!((record.count, record.first_nanos, record.last_nanos), (1, 7, 7));
        assert!(irq.take().is_none(), "the same message was answered twice");
        assert!(!irq.armed(), "a drained record still reads ready");
    });
}
