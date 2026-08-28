#![warn(clippy::undocumented_unsafe_blocks)]
//! The machine, and the only part of this kernel that knows which one.
//!
//! Every `unsafe` block here carries a one-line `SAFETY:` comment, enforced by the lint above.
//! [`percpu`] owns every `gs:` access; nothing outside this directory writes one.

pub mod apic;
pub mod control_regs;
pub mod cpu;
pub mod entry;
pub mod fpu;
pub mod idt;
pub mod mtrr;
pub mod pat;
pub mod percpu;
pub mod smp;
pub mod syscall;
pub mod tlb;

/// One log reservation and its publication, atomic against an interrupt on this CPU. TF is always clear in Ring 0, so this guard leaves it alone.
#[must_use = "dropping the log commit guard reopens interrupts and single-step traps"]
pub(crate) struct LogCommitGuard {
    rflags: u64,
    // Same-CPU only: keeps this guard `!Send + !Sync`.
    _not_send_sync: core::marker::PhantomData<*mut ()>,
}

impl LogCommitGuard {
    pub fn close() -> Self {
        let rflags: u64;
        // SAFETY: pushfq/pop is balanced; cli touches only RFLAGS — one uninterruptible read-and-clear of IF.
        unsafe {
            // No `nomem`: the clobber keeps shard selection and publication on the closed side.
            core::arch::asm!(
                "pushfq",
                "pop {saved}",
                saved = out(reg) rflags,
            );
            // Skips `cli` under the `log-unbracketed-reserve` actuator, to stage that defect.
            if crate::actuator::log_unbracketed_reserve() {
                return Self { rflags, _not_send_sync: core::marker::PhantomData };
            }
            core::arch::asm!("cli");
        }
        Self { rflags, _not_send_sync: core::marker::PhantomData }
    }
}

impl Drop for LogCommitGuard {
    fn drop(&mut self) {
        // SAFETY: `close`'s argument, restored on the CPU that captured it.
        unsafe {
            // No `nomem`: the slot store must stay before this reopens IF.
            core::arch::asm!(
                "push {saved}",
                "popfq",
                saved = in(reg) self.rflags,
            );
        }
    }
}

/// Adds one to `counter`, atomic against an interrupt on this CPU, and answers the value before the add.
/// # Safety: `counter` is written by no other CPU; `guard` covers the shard selection that owns it.
#[inline(always)]
pub unsafe fn percpu_fetch_add(
    counter: &core::sync::atomic::AtomicU64,
    _guard: &LogCommitGuard,
) -> u64 {
    // Under `log-shared-reservation`, stage a load/store race instead of the `xadd` below.
    if crate::actuator::log_shared_reservation() {
        let previous = counter.load(core::sync::atomic::Ordering::Relaxed);
        if crate::log::nested::inject() {
            // SAFETY: `sti`/`cli` each write one `RFLAGS` bit and touch no memory.
            unsafe {
                core::arch::asm!("sti");
                for _ in 0..256 {
                    core::hint::spin_loop();
                }
                core::arch::asm!("cli");
            }
        }
        counter.store(previous + 1, core::sync::atomic::Ordering::Relaxed);
        return previous;
    }

    let previous: u64;
    // Not `AtomicU64::fetch_add`: its locked xadd is costly under QEMU TCG emulation.
    // SAFETY: `counter.as_ptr()` is live; unlocked `xadd` retires whole, atomic against an interrupt here.
    unsafe {
        // No `preserves_flags`: `xadd` changes arithmetic flags.
        core::arch::asm!(
            "xadd [{ptr}], {out}",
            ptr = in(reg) counter.as_ptr(),
            out = inout(reg) 1u64 => previous,
        );
    }
    previous
}
