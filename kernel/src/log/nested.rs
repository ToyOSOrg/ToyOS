//! Injects a self-IPI from inside `emit`.
//!
//! `log-nested-emit`: mid body-copy — overwrites an already-published slot,
//! indistinguishable from drop-oldest.

/// Producer id the burst's records use — one the log gate's own producer never takes.
#[cfg(feature = "boot-actuators")]
pub const NEST_PRODUCER: u64 = u64::MAX;

#[cfg(feature = "boot-actuators")]
mod armed {
    use core::sync::atomic::{AtomicBool, Ordering};

    use crate::log::shard::SHARD_RECORDS;

    /// One-shot for the body-copy injection point, consumed by `mid_body`.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Set by the injection and cleared by the handler, so a delivery for any other reason emits nothing.
    static OWED: AtomicBool = AtomicBool::new(false);

    static STARTED: AtomicBool = AtomicBool::new(false);

    /// Spin count after sending the IPI, so delivery lands inside the window rather than after it.
    const WINDOW: usize = 256;

    pub fn start_once() {
        if STARTED.swap(true, Ordering::Relaxed) {
            return;
        }
        crate::log!("lognest start records={SHARD_RECORDS}");
        // `IF` is clear for a whole syscall, so injecting with it clear would never test the guard; Ring 0 is not preempted, so the body runs whole on this CPU.
        crate::arch::cpu::enable_interrupts();
        body();
        crate::arch::cpu::disable_interrupts();
    }

    fn body() {
        ARMED.store(true, Ordering::Relaxed);
        crate::log!("lognest outer, and an interrupt is due inside this record's body");
        // Reset unconditionally: the one-shot must not outlive this record, or a later injection would land in an unrelated log line.
        ARMED.store(false, Ordering::Relaxed);
        crate::log!("lognest done emitted={SHARD_RECORDS}");
    }

    /// Consumes the one-shot and sends this CPU its own IPI; `true` if this call sent it.
    fn inject() -> bool {
        if !ARMED.swap(false, Ordering::Relaxed) {
            return false;
        }
        OWED.store(true, Ordering::Relaxed);
        crate::arch::irqchip::send_self(crate::arch::trap::LOG_NEST_VECTOR);
        true
    }

    /// Injection point inside the body copy; spins after sending so delivery lands inside it.
    pub fn mid_body() {
        if !inject() {
            return;
        }
        for _ in 0..WINDOW {
            core::hint::spin_loop();
        }
    }

    /// Emits exactly one shard generation — the count that reads the outer record's disappearance as drop-oldest, not corruption.
    pub fn deliver() {
        if !OWED.swap(false, Ordering::Relaxed) {
            return;
        }
        for index in 0..SHARD_RECORDS as u64 {
            crate::log::storm::emit_patterned(super::NEST_PRODUCER, index);
        }
    }
}

/// Arms the injection and emits the record it lands in, inline in the calling `SYS_LOG_READ`, once; compiled only under `boot-actuators`.
#[cfg(feature = "boot-actuators")]
pub fn start_once() {
    armed::start_once();
}

/// Injection point halfway through a record's body copy; always compiled so `kernel-loom`'s separate copy of `log::shard` names one path.
pub fn mid_body() {
    #[cfg(feature = "boot-actuators")]
    armed::mid_body();
}

/// The `log_nest` interrupt handler's body; no shipping kernel installs that handler.
#[cfg(feature = "boot-actuators")]
pub fn deliver() {
    #[cfg(feature = "boot-actuators")]
    armed::deliver();
}
