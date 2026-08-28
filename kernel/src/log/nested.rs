//! Injects a self-IPI from inside `emit`, landing at one of two windows
//! depending on which actuator is armed.
//!
//! `log-nested-emit`: mid body-copy — overwrites an already-published slot,
//! indistinguishable from drop-oldest.
//! `log-nested-reserve`: between the shard-pointer read and the `xadd` —
//! produces a shard whose `at_ns` descends, which `Descent::advance` and the
//! log gate assume cannot happen.

/// Producer id the burst's records use — outside the range any real storm thread can have.
#[cfg(feature = "boot-actuators")]
pub const NEST_PRODUCER: u64 = u64::MAX;

#[cfg(feature = "boot-actuators")]
mod armed {
    use core::sync::atomic::{AtomicBool, Ordering};

    use crate::log::shard::SHARD_RECORDS;
    use crate::sched::kthread::{self, OnPanic};

    /// One-shot for the body-copy injection point, consumed by `mid_body` or, under `log-shared-reservation`, by the outer `inject`.
    static ARMED: AtomicBool = AtomicBool::new(false);

    /// One-shot for the reservation-window injection point, consumed by [`reserve_window`]; kept separate from `ARMED` so it can't starve the body window.
    static ARMED_RESERVE: AtomicBool = AtomicBool::new(false);

    /// Set by the injection and cleared by the handler, so a delivery for any other reason emits nothing.
    static OWED: AtomicBool = AtomicBool::new(false);

    static STARTED: AtomicBool = AtomicBool::new(false);

    /// Spin count after sending the IPI, so delivery lands inside the window rather than after it.
    const WINDOW: usize = 256;

    pub fn start_once() {
        // Both actuators name the same injection; arming both would inject into one record twice.
        assert!(
            !(crate::actuator::log_nested_emit() && crate::actuator::log_nested_reserve()),
            "log-nested-emit and log-nested-reserve both name the one injection this thread arms"
        );
        if STARTED.swap(true, Ordering::Relaxed) {
            return;
        }
        crate::log!("lognest start records={SHARD_RECORDS}");
        // A kernel thread, not the syscall that arms it: `IF` is clear for a whole syscall, so injecting there would never test the guard.
        // `Halt`: this thread carries the whole stimulus; surviving its death would answer the gate having injected nothing.
        kthread::spawn("lognest", body, 0, OnPanic::Halt);
    }

    extern "C" fn body(_arg: u64) -> ! {
        if crate::actuator::log_nested_reserve() {
            ARMED_RESERVE.store(true, Ordering::Relaxed);
            crate::log!(
                "lognest outer, and an interrupt is due between this record's shard read and its \
                 xadd"
            );
            ARMED_RESERVE.store(false, Ordering::Relaxed);
        } else {
            ARMED.store(true, Ordering::Relaxed);
            crate::log!("lognest outer, and an interrupt is due inside this record's body");
            // Reset unconditionally: the one-shot must not outlive this record, or a later injection would land in an unrelated log line.
            ARMED.store(false, Ordering::Relaxed);
        }
        crate::log!("lognest done emitted={SHARD_RECORDS}");

        crate::completion::park_forever();
    }

    /// Consumes the one-shot and sends this CPU its own IPI; `true` if this call sent it.
    pub fn inject() -> bool {
        if !ARMED.swap(false, Ordering::Relaxed) {
            return false;
        }
        OWED.store(true, Ordering::Relaxed);
        crate::arch::apic::send_self(crate::arch::idt::LOG_NEST_VECTOR);
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

    /// Injection point between the shard-pointer read and the `xadd`; consumes `ARMED_RESERVE` directly, not via `inject`.
    pub fn reserve_window() {
        if !ARMED_RESERVE.swap(false, Ordering::Relaxed) {
            return;
        }
        OWED.store(true, Ordering::Relaxed);
        crate::arch::apic::send_self(crate::arch::idt::LOG_NEST_VECTOR);
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

/// Arms the injection on a dedicated kernel thread, once; compiled only under `boot-actuators`.
#[cfg(feature = "boot-actuators")]
pub fn start_once() {
    #[cfg(feature = "boot-actuators")]
    armed::start_once();
}

/// Consumes the one-shot at the reservation, for `log-shared-reservation`; `true` if an IPI went out.
pub fn inject() -> bool {
    #[cfg(feature = "boot-actuators")]
    return armed::inject();
    #[cfg(not(feature = "boot-actuators"))]
    false
}

/// Injection point halfway through a record's body copy; always compiled so `kernel-loom`'s separate copy of `log::shard` names one path.
pub fn mid_body() {
    #[cfg(feature = "boot-actuators")]
    armed::mid_body();
}

/// Injection point between a record's shard-pointer read and its `xadd`; called only from `arch::percpu::reserve_log_slot`.
pub fn reserve_window() {
    #[cfg(feature = "boot-actuators")]
    armed::reserve_window();
}

/// The `log_nest` interrupt handler's body; no shipping kernel installs that handler.
#[cfg(feature = "boot-actuators")]
pub fn deliver() {
    #[cfg(feature = "boot-actuators")]
    armed::deliver();
}
