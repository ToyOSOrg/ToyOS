//! Loom: a claimed function's interrupt ends masked exactly while its record
//! owes a mask, from the second unread message, however the handler and a
//! read interleave (`pcidev/record.rs`); the `device-irq-gate-intent` control
//! must red the two-message model.

use kernel_loom::device_irq::{Gate, Interrupt};
use loom::sync::{Arc, Mutex};

/// The claim's interrupt as hardware has it, behind the gate's lock.
struct Line(Mutex<bool>);

impl Gate for Line {
    fn hold(&self, masked: impl FnOnce() -> bool) {
        let mut hardware = self.0.lock().unwrap();
        *hardware = masked();
    }
}

/// Messages against one read that may run at any point among them, and a
/// last read once they have all landed: after each, hardware is masked
/// exactly while the record owes it.
fn model(messages: u64) {
    loom::model(move || {
        let irq = Arc::new(Interrupt::new());
        let line = Arc::new(Line(Mutex::new(false)));

        let isr = {
            let (irq, line) = (irq.clone(), line.clone());
            loom::thread::spawn(move || {
                for at in 1..=messages {
                    irq.took_masking(at, &*line);
                }
            })
        };

        let taken = irq.take_opening(&*line).map_or(0, |record| u64::from(record.count));
        isr.join().unwrap();
        let masked = *line.0.lock().unwrap();
        assert_eq!(
            masked,
            irq.owes_mask(),
            "the read took {taken} of {messages} message(s) and hardware is {} where the record owes it {}",
            if masked { "masked" } else { "open" },
            if irq.owes_mask() { "masked" } else { "open" },
        );

        let left = irq.take_opening(&*line).map_or(0, |record| u64::from(record.count));
        assert_eq!(taken + left, messages, "the reads took {taken} and {left} of {messages}");
        assert!(!*line.0.lock().unwrap(), "every message was read and the claim is still masked");
    });
}

/// One message per read never masks.
#[test]
fn one_message_per_read_never_masks() {
    model(1);
}

/// A second message before a read masks, and the read that takes them opens
/// it, wherever the read lands among them.
#[test]
fn hardware_ends_as_the_record_owes_after_two_messages() {
    model(2);
}
