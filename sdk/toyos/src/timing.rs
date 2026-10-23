//! Timing protection for the calling thread, scoped. [`protect`] turns on the
//! CPU's data operand independent timing mode where it has one and answers a
//! [`Protection`] that puts back the state it found when it drops, so
//! protections nest and an early return or an unwinding panic restores the
//! state. Any program may use it; it asks for no right.
//!
//! The mode makes the instructions its vendor lists take time independent of
//! their data operands, and nothing more: code must already be constant-time
//! in its branches, addresses and choice of instructions for the mode to mean
//! anything. Where the machine has no such mode, [`Protection::is_active`] is
//! false; nothing here reports a machine without one as protected.
//!
//! On x86-64 the mode is Intel's DOITM, which the kernel switches
//! (`SYS_TIMING_PROTECTION`): turning it on and off costs a syscall each way.
//! On AArch64 it is `PSTATE.DIT`, which this thread sets itself. A protection
//! made while one is active costs a load on either. So a caller doing many
//! small secret operations in a row holds one protection around them all.
//!
//! Protections drop in the reverse of the order they were made, as scopes
//! end: dropping an outer one first ends the protection while an inner one
//! is alive. A protection is its thread's, so it is not `Send`.
//!
//! On x86-64 this module remembers the state in the thread's control block
//! (`toyos_abi::tcb::X86_64_TIMING`) and is its one writer: a raw
//! `SYS_TIMING_PROTECTION` that changed it panics this module's next switch.

use core::marker::PhantomData;

/// The calling thread's timing protection until this drops. See the module.
#[must_use = "the protection ends when this drops"]
pub struct Protection {
    active: bool,
    /// Whether the mode was off before this, so this turns it off again.
    restore_off: bool,
    _thread: PhantomData<*const ()>,
}

impl Protection {
    /// Whether the mode is on for this protection's life: false where the
    /// machine has none.
    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// Turn the calling thread's timing protection on where the machine has it,
/// until the answer drops.
pub fn protect() -> Protection {
    let (active, restore_off) = match arch::enable() {
        Some(was_on) => (true, !was_on),
        None => (false, false),
    };
    Protection { active, restore_off, _thread: PhantomData }
}

impl Drop for Protection {
    fn drop(&mut self) {
        if self.restore_off {
            arch::disable();
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

    use toyos_abi::syscall::{timing_protection, SyscallError, TimingRequest};
    use toyos_abi::tcb::X86_64_TIMING;

    /// Whether the machine has DOITM, once a thread has asked: a machine
    /// fact, so every copy of this crate may keep its own.
    static SUPPORT: AtomicU8 = AtomicU8::new(UNKNOWN);
    const UNKNOWN: u8 = 0;
    const ABSENT: u8 = 1;
    const PRESENT: u8 = 2;

    fn held() -> bool {
        let word: u32;
        // SAFETY: `fs` is this thread's TP and `TP + X86_64_TIMING` is a word
        // of the TCB the kernel reserves there, which this module alone uses.
        unsafe {
            core::arch::asm!(
                "mov {word:e}, dword ptr fs:[{at}]",
                word = out(reg) word,
                at = const X86_64_TIMING,
                options(nostack, readonly, preserves_flags),
            );
        }
        word != 0
    }

    fn hold(on: bool) {
        // SAFETY: as `held`'s.
        unsafe {
            core::arch::asm!(
                "mov dword ptr fs:[{at}], {word:e}",
                word = in(reg) u32::from(on),
                at = const X86_64_TIMING,
                options(nostack, preserves_flags),
            );
        }
    }

    /// Whether the mode was on before; `None` where the machine has none.
    pub(super) fn enable() -> Option<bool> {
        if held() {
            return Some(true);
        }
        if SUPPORT.load(Relaxed) == ABSENT {
            return None;
        }
        match timing_protection(TimingRequest::On) {
            Ok(was_on) => {
                assert!(!was_on, "toyos::timing: DOITM was on behind this thread's protections");
                SUPPORT.store(PRESENT, Relaxed);
                hold(true);
                Some(false)
            }
            Err(SyscallError::NotSupported) => {
                SUPPORT.store(ABSENT, Relaxed);
                None
            }
            Err(e) => panic!("toyos::timing: SYS_TIMING_PROTECTION refused turning DOITM on: {e}"),
        }
    }

    pub(super) fn disable() {
        match timing_protection(TimingRequest::Off) {
            Ok(was_on) => assert!(was_on, "toyos::timing: DOITM was off behind this thread's protection"),
            Err(e) => panic!("toyos::timing: SYS_TIMING_PROTECTION refused turning DOITM off: {e}"),
        }
        hold(false);
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

    use toyos_abi::syscall::{timing_protection, SyscallError, TimingRequest};

    /// Whether the machine has FEAT_DIT, once a thread has asked the kernel:
    /// `MSR DIT` is undefined without it.
    static SUPPORT: AtomicU8 = AtomicU8::new(UNKNOWN);
    const UNKNOWN: u8 = 0;
    const ABSENT: u8 = 1;
    const PRESENT: u8 = 2;

    /// `PSTATE.DIT` in the `DIT` register's view.
    const DIT: u64 = 1 << 24;

    fn supported() -> bool {
        match SUPPORT.load(Relaxed) {
            PRESENT => true,
            ABSENT => false,
            _ => {
                let present = match timing_protection(TimingRequest::Query) {
                    Ok(_) => true,
                    Err(SyscallError::NotSupported) => false,
                    Err(e) => panic!("toyos::timing: SYS_TIMING_PROTECTION refused a query: {e}"),
                };
                SUPPORT.store(if present { PRESENT } else { ABSENT }, Relaxed);
                present
            }
        }
    }

    /// `DIT` as `S3_3_C4_C2_5`, which assembles without FEAT_DIT's name.
    fn read() -> bool {
        let value: u64;
        // SAFETY: `DIT` is readable at EL0 on a CPU with FEAT_DIT, which
        // `supported` established.
        unsafe { core::arch::asm!("mrs {}, s3_3_c4_c2_5", out(reg) value, options(nomem, nostack, preserves_flags)) };
        value & DIT != 0
    }

    fn write(on: bool) {
        // SAFETY: as `read`'s; the write changes `PSTATE.DIT` alone.
        unsafe {
            core::arch::asm!("msr s3_3_c4_c2_5, {}", in(reg) if on { DIT } else { 0 }, options(nomem, nostack, preserves_flags))
        };
    }

    pub(super) fn enable() -> Option<bool> {
        if !supported() {
            return None;
        }
        let was_on = read();
        if !was_on {
            write(true);
        }
        Some(was_on)
    }

    pub(super) fn disable() {
        write(false);
    }
}
