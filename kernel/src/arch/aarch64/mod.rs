#![warn(clippy::undocumented_unsafe_blocks)]
//! AArch64: the Arm A-profile at EL1, found and described through ACPI.
//!
//! Every `unsafe` block here carries a one-line `SAFETY:` comment, enforced by the lint above.

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
pub mod pmu;
pub mod power;
pub mod psci;
pub mod rtc;
pub mod smp;
pub mod switch;
pub mod syscall;
pub mod tlb;
pub mod trap;
pub mod watchdog;

/// The machine every program image this kernel loads must be built for.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::Aarch64;

/// A message-signalled interrupt's address and data for `vector` on CPU
/// `dest`, which this machine does not give: the doorbell is an ITS's
/// `GITS_TRANSLATER` and the data an event the ITS maps, and nothing here
/// drives an ITS. Every function that would take one is a driver the
/// small-kernel track moves out of the kernel, or a claimed function the
/// SMMUv3 of the port's stage 6 must translate first; each is refused by name.
pub fn msi_message(_dest: u32, _vector: u8) -> Result<(u32, u32), &'static str> {
    Err("AArch64 delivers no message-signalled interrupt to this kernel: the GICv3 ITS is unported")
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
    // A load and a store, not an atomic add: the guard masks the only other
    // writer this CPU has, and no other CPU writes the counter.
    counter.store(previous + 1, core::sync::atomic::Ordering::Relaxed);
    previous
}
