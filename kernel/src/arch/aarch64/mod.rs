#![warn(clippy::undocumented_unsafe_blocks)]
//! AArch64: the Arm A-profile at EL1, found and described through ACPI.
//!
//! Every `unsafe` block here carries a one-line `SAFETY:` comment, enforced by the lint above.
//!
//! **What exists and what is owed.** The boot reaches its console: the entry
//! drops from EL2, applies the control-register declaration, turns on the
//! loader's tables and installs the exception vectors; the PL011 is found
//! through SPCR. Everything the kernel does after the console — the interrupt
//! controller, the timer, its own page tables, other CPUs, user mode — is
//! owed by a stage of the port (`issues/kernel/toyos-runs-on-arm64.md`), and
//! each item that stands for it here is an [`owed!`] that panics naming it. A kernel that reaches
//! one stops loudly on its panel; none of them returns a guess.

/// Stands for work the port owes: panics naming what and which stage of the
/// track owns it (`issues/kernel/toyos-runs-on-arm64.md`), or that none does yet.
macro_rules! owed {
    ($what:literal, $stage:literal) => {
        panic!(concat!("aarch64: ", $what, ": owed by ", $stage))
    };
}

pub mod barrier;
pub mod boot;
pub mod cache;
pub mod console_uart;
pub mod control_regs;
pub mod cpu;
pub mod entropy;
pub mod entry;
pub mod fpu;
pub mod hw;
pub mod iommu_unit;
pub mod irqchip;
pub mod keyboard_controller;
pub mod paging;
pub mod percpu;
pub mod pio;
pub mod pmu;
pub mod rtc;
pub mod smp;
pub mod syscall;
pub mod tlb;
pub mod trap;
pub mod watchdog;

/// The machine every program image this kernel loads must be built for.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::Aarch64;

/// A message-signalled interrupt's address and data for `vector` on CPU
/// `dest`. On AArch64 the doorbell is an ITS's `GITS_TRANSLATER`, one per ITS
/// the MADT names, and the data is an event the ITS maps: stage 4 builds both.
pub fn msi_message(_dest: u32, _vector: u8) -> (u32, u32) {
    owed!("an MSI doorbell (the GICv3 ITS)", "stage 4")
}

/// Interrupts masked on this CPU for as long as the guard lives, and then put
/// back as they were — restored, not enabled — so a guard nests inside a region
/// that is already masked. `DAIF`'s `I` and `F` are what it closes; `D` and `A`
/// are the boot's and it leaves them alone.
///
/// Both edges are compiler barriers (no `nomem`): a memory access written
/// inside the region is emitted inside it.
#[must_use = "dropping the guard reopens interrupts"]
pub struct IrqGuard {
    daif: u64,
    // Same-CPU only: keeps this guard `!Send + !Sync`.
    _not_send_sync: core::marker::PhantomData<*mut ()>,
}

impl IrqGuard {
    pub fn close() -> Self {
        let daif: u64;
        // SAFETY: reads `DAIF` and sets its `I` and `F` bits; touches nothing else.
        unsafe {
            core::arch::asm!("mrs {saved}, daif", "msr daifset, #3", saved = out(reg) daif);
        }
        Self { daif, _not_send_sync: core::marker::PhantomData }
    }

    /// The mask captured and interrupts left as they are: what the
    /// `log-unbracketed-reserve` actuator stages a log reservation with.
    #[cfg(feature = "boot-actuators")]
    pub fn unclosed() -> Self {
        let daif: u64;
        // SAFETY: reads `DAIF` and writes nothing.
        unsafe {
            core::arch::asm!("mrs {saved}, daif", saved = out(reg) daif);
        }
        Self { daif, _not_send_sync: core::marker::PhantomData }
    }
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        // SAFETY: the word `close` read out of `DAIF` on this CPU (the guard is
        // `!Send`), restored whole.
        unsafe {
            core::arch::asm!("msr daif, {saved}", saved = in(reg) self.daif);
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
    let previous = counter.load(core::sync::atomic::Ordering::Relaxed);
    // Under `log-shared-reservation`, open the window the guard closes, so a
    // nested record can land between the load and the store.
    if crate::actuator::log_shared_reservation() && crate::log::nested::inject() {
        // SAFETY: each writes `DAIF.I` and touches no memory.
        unsafe {
            core::arch::asm!("msr daifclr, #2");
            for _ in 0..256 {
                core::hint::spin_loop();
            }
            core::arch::asm!("msr daifset, #2");
        }
    }
    // A load and a store, not an atomic add: the guard masks the only other
    // writer this CPU has, and no other CPU writes the counter.
    counter.store(previous + 1, core::sync::atomic::Ordering::Relaxed);
    previous
}
