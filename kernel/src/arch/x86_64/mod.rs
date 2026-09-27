#![warn(clippy::undocumented_unsafe_blocks)]
//! x86-64: the PC this kernel was first written for.
//!
//! Every `unsafe` block here carries a one-line `SAFETY:` comment, enforced by the lint above.
//! [`percpu`] owns every `gs:` access; nothing outside this directory writes one.
//!
//! The PC platform's own devices live here too, because no other architecture
//! has them: the i8042, the I/O APIC, the CMOS RTC, the chipset's TCO
//! watchdog and VT-d. Generic code reaches each through the concept it serves
//! (`keyboard_controller`, `watchdog`, `iommu_unit`, …), never by its name.

pub mod apic;
pub mod barrier;
pub mod boot;
pub mod cache;
pub mod control_regs;
pub mod console_uart;
pub mod cpu;
pub mod entropy;
pub mod entry;
pub mod fpu;
pub mod hpet;
pub mod hw;
pub mod i8042;
pub mod idt;
pub mod ioapic;
pub mod mtrr;
#[cfg(feature = "boot-actuators")]
pub mod nmi_gate;
pub mod paging;
pub mod pat;
pub mod percpu;
pub mod pio;
pub mod pmu;
pub mod rtc;
pub mod smp;
pub mod switch;
pub mod syscall;
pub mod tlb;
pub mod vtd;
pub mod watchdog;

pub use apic as irqchip;
pub use i8042 as keyboard_controller;
pub use idt as trap;
pub use vtd as iommu_unit;

pub use apic::{msi_message, MSI_DOORBELL};

/// The machine every program image this kernel loads must be built for.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::X86_64;

/// Interrupts masked on this CPU for as long as the guard lives, and then put
/// back as they were — restored, not enabled — so a guard nests inside a region
/// that is already masked. The one way this kernel masks and restores: the
/// scheduler's pass, a log record's reservation and publication, and the
/// console backend each hold one. `TF` is always clear in Ring 0, so the guard
/// leaves it alone.
///
/// Both edges are compiler barriers (no `nomem`): a memory access written
/// inside the region is emitted inside it.
#[must_use = "dropping the guard reopens interrupts"]
pub struct IrqGuard {
    rflags: u64,
    // Same-CPU only: keeps this guard `!Send + !Sync`.
    _not_send_sync: core::marker::PhantomData<*mut ()>,
}

impl IrqGuard {
    pub fn close() -> Self {
        let rflags: u64;
        // SAFETY: pushfq/pop is balanced and cli touches only RFLAGS — one
        // uninterruptible read-and-clear of IF.
        unsafe {
            core::arch::asm!("pushfq", "pop {saved}", "cli", saved = out(reg) rflags);
        }
        Self { rflags, _not_send_sync: core::marker::PhantomData }
    }

    /// The flags captured and interrupts left as they are: what the
    /// `log-unbracketed-reserve` actuator stages a log reservation with.
    #[cfg(feature = "boot-actuators")]
    pub fn unclosed() -> Self {
        let rflags: u64;
        // SAFETY: pushfq/pop is balanced and writes no RFLAGS bit.
        unsafe {
            core::arch::asm!("pushfq", "pop {saved}", saved = out(reg) rflags);
        }
        Self { rflags, _not_send_sync: core::marker::PhantomData }
    }
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        // SAFETY: the word `close` read out of RFLAGS on this CPU (the guard is
        // `!Send`), restored whole.
        unsafe {
            core::arch::asm!("push {saved}", "popfq", saved = in(reg) self.rflags);
        }
    }
}

/// Adds one to `counter`, atomic against an interrupt on this CPU, and answers the value before the add.
/// # Safety: `counter` is written by no other CPU; `guard` covers the shard selection that owns it.
#[inline(always)]
pub unsafe fn percpu_fetch_add(
    counter: &core::sync::atomic::AtomicU64,
    _guard: &IrqGuard,
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
