#![warn(clippy::undocumented_unsafe_blocks)]
//! x86-64: the PC this kernel was first written for.
//!
//! Every `unsafe` block here carries a one-line `SAFETY:` comment, enforced by the lint above.
//! [`percpu`] owns every `gs:` access; nothing outside this directory writes one.
//!
//! The PC platform's own devices live here too, because no other architecture
//! has them: the I/O APIC, the CMOS RTC, the chipset's watchdog, VT-d and
//! AMD-Vi. Generic code reaches each through the concept it serves
//! (`watchdog`, `iommu_unit`, …), never by its name.

/// The interrupt sources only this architecture counts, handed to the
/// census's list (`irq_census`).
macro_rules! census_sources {
    ($list:ident) => {
        $list! {
            /// Vector 0x26, the RTC's update-ended interrupt, once a second.
            Rtc => "rtc",
        }
    };
}
pub(crate) use census_sources;

pub mod acpi_mode;
pub mod amdvi;
pub mod apic;
pub mod barrier;
pub mod boot;
pub mod cache;
pub mod clocksource;
pub mod control_regs;
pub mod copy;
pub mod counters;
pub mod console_uart;
pub mod cpu;
pub mod entropy;
pub mod entry;
pub mod fpu;
pub mod hw;
pub mod idle;
pub mod idt;
pub mod iommu_unit;
pub mod ioapic;
pub mod microcode;
pub mod mtrr;
pub mod paging;
pub mod pat;
pub mod percpu;
pub mod pio;
pub mod pmu;
pub mod power;
pub mod rtc;
pub mod smi_cmd;
pub mod smp;
pub mod switch;
pub mod syscall;
pub mod tlb;
pub mod vtd;
pub mod watchdog;

pub use apic as irqchip;
pub use idt as trap;
pub use iommu_unit::interrupt as message_unit;

pub use apic::MSI_DOORBELL;

/// The machine every program image this kernel loads must be built for.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::X86_64;

/// Interrupts masked on this CPU for as long as the guard lives, and then put
/// back as they were — restored, not enabled — so a guard nests inside a region
/// that is already masked. The one way this kernel masks and restores. `TF` is
/// always clear in Ring 0, so the guard leaves it alone.
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
        #[cfg(feature = "mask-windows")]
        if idt::frame_interrupts_enabled(rflags) {
            crate::windows::irqs_masked();
        }
        Self { rflags, _not_send_sync: core::marker::PhantomData }
    }
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        // Only where the restore opens them.
        #[cfg(feature = "mask-windows")]
        if idt::frame_interrupts_enabled(self.rflags) && !cpu::interrupts_enabled() {
            crate::windows::irqs_unmasking();
        }
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

/// This architecture's part of a CPU's `cpu_local` record.
pub struct CpuLocal {
    pub counters: counters::Local,
    pub clock: clocksource::Local,
    pub idle_line: idle::Line,
    #[cfg(feature = "switch-witness")]
    pub hw: hw::Local,
}

impl CpuLocal {
    pub fn new() -> Self {
        Self {
            counters: counters::Local::new(),
            clock: clocksource::Local::new(),
            idle_line: idle::Line::new(),
            #[cfg(feature = "switch-witness")]
            hw: hw::Local::new(),
        }
    }
}

/// What this architecture keeps for each of the `cpus` CPUs the firmware
/// names outside that record: the TLB's and the SMI command port's rounds.
pub fn size(cpus: usize) {
    tlb::size(cpus);
    smi_cmd::size(cpus);
    clocksource::size(cpus);
}
