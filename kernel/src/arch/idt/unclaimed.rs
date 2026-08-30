//! Every vector no `idt_vectors!` row claims — one gate that counts and names
//! it, since a `P = 0` slot escalates to `#DF` and halts the machine namelessly.
//!
//! The EOI is conditional, as the spurious handler's is (SDM Vol. 3A §11.9): a
//! genuinely spurious delivery sets no ISR bit, so the handler asks before it
//! acknowledges. One gate serves every slot, so the vector's identity is the
//! highest in-service bit (SDM Vol. 3A §12.8.4), and a no-ISR delivery is
//! counted apart with no vector to blame. Does not log — this handler can run
//! inside the log's own commit bracket.

use core::arch::naked_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::apic;

/// One bit per vector taken through this gate; [`log_vectors`] reads it back.
static TAKEN: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// Deliveries that set no ISR bit, so there is no vector to blame.
static NO_ISR: AtomicU64 = AtomicU64::new(0);
/// Event count at the last print, so process exit logs once per batch.
static REPORTED: AtomicU64 = AtomicU64::new(0);

#[unsafe(naked)]
pub(super) extern "sysv64" fn unclaimed_entry() {
    naked_asm!(
        // Not `ring3_naked_asm!`: this vector can interrupt any instruction,
        // `memmove`'s `std` … `cld` window included, so it owes its own `cld`.
        "cld",
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push rbp",
        "mov rbp, rsp",
        "and rsp, -16",
        "call {took}",
        "mov rsp, rbp",
        "pop rbp",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "iretq",
        took = sym took,
    );
}

/// Counts the delivery, remembers which vector it was, and acknowledges it
/// only if the ISR shows it needs one.
extern "sysv64" fn took() {
    crate::irq_census::irq_took!(Unclaimed);
    match apic::in_service_highest() {
        Some(vector) => {
            TAKEN[(vector >> 6) as usize].fetch_or(1 << (vector & 63), Ordering::Relaxed);
            apic::eoi();
        }
        None => {
            NO_ISR.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Whether `vector` has been taken through this gate.
#[cfg(feature = "boot-actuators")]
pub fn was_taken(vector: u8) -> bool {
    TAKEN[(vector >> 6) as usize].load(Ordering::Relaxed) & (1 << (vector & 63)) != 0
}

/// Logs which vectors the gate absorbed, at process exit beside the censuses,
/// once per batch of new events; a boot that staged nothing says nothing.
pub fn log_vectors() {
    let words = [
        TAKEN[0].load(Ordering::Relaxed),
        TAKEN[1].load(Ordering::Relaxed),
        TAKEN[2].load(Ordering::Relaxed),
        TAKEN[3].load(Ordering::Relaxed),
    ];
    let no_isr = NO_ISR.load(Ordering::Relaxed);
    let events = words.iter().map(|w| w.count_ones() as u64).sum::<u64>() + no_isr;
    if events == 0 || REPORTED.swap(events, Ordering::Relaxed) == events {
        return;
    }
    struct Vectors([u64; 4]);
    impl core::fmt::Display for Vectors {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for vector in 0..256usize {
                if self.0[vector >> 6] & (1 << (vector & 63)) != 0 {
                    write!(f, " {vector:#04x}")?;
                }
            }
            Ok(())
        }
    }
    crate::log!("irq: unclaimed vectors{} no-isr={no_isr}", Vectors(words));
}

/// Raises a vector no row claims on this CPU and verifies the gate counted
/// it, remembered it, acknowledged it, and left the CPU taking interrupts.
#[cfg(feature = "boot-actuators")]
pub fn selftest() {
    use crate::irq_census::Source;

    /// Unfilled by every `idt_vectors!` row and by the actuator gate at 0x27.
    const PROBE_VECTOR: u8 = 0x30;

    let cpu = crate::arch::percpu::cpu_id();
    let Some(before) = crate::irq_census::deliveries(cpu, Source::Unclaimed) else {
        crate::log!("LAPIC: unclaimed selftest FAILED — this CPU publishes no census block");
        return;
    };
    let taken_before = crate::irq_census::deliveries_total(cpu).unwrap_or(0);

    apic::send_self(PROBE_VECTOR);

    const ARRIVES: crate::time::Budget = crate::time::Budget::of(
        crate::time::Duration::from_millis(50),
        "the delivery is reported as never having arrived",
    );
    let delivered = crate::clock::settles(ARRIVES.nanos(), || {
        crate::irq_census::deliveries(cpu, Source::Unclaimed).unwrap_or(before) > before
    });
    if !delivered {
        crate::log!("LAPIC: unclaimed selftest FAILED — vector {PROBE_VECTOR:#x} never arrived");
        return;
    }

    if !was_taken(PROBE_VECTOR) {
        crate::log!(
            "LAPIC: unclaimed selftest FAILED — vector {PROBE_VECTOR:#x} was counted and not \
             remembered, so no report can name it"
        );
        return;
    }

    // A missing EOI leaves the ISR bit set, starving every vector at or below
    // its priority class (SDM Vol. 3A §11.8.4).
    if apic::in_service(PROBE_VECTOR) {
        crate::log!(
            "LAPIC: unclaimed selftest FAILED — vector {PROBE_VECTOR:#x} is still in service on \
             cpu{cpu}"
        );
        return;
    }

    let ran_on = crate::clock::settles(ARRIVES.nanos(), || {
        crate::irq_census::deliveries_total(cpu).unwrap_or(taken_before) > taken_before
    });
    if !ran_on {
        crate::log!(
            "LAPIC: unclaimed selftest FAILED — cpu{cpu} took no interrupt at all after the \
             unclaimed one"
        );
        return;
    }

    let after = crate::irq_census::deliveries(cpu, Source::Unclaimed).unwrap_or(before);
    crate::log!(
        "LAPIC: unclaimed selftest 3/3 — vector {:#x} delivered on cpu{} ({} -> {}), remembered, \
         acknowledged, and the CPU took interrupts after it",
        PROBE_VECTOR,
        cpu,
        before,
        after,
    );
}
