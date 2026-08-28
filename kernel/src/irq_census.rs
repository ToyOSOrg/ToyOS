//! Per-CPU, per-source interrupt delivery counts, kept in `PerCpu` for a lock-free `add`.
//! `total` increments separately from the per-source counts, so a missing source
//! increment shows as `total` exceeding their sum; `irq_census_conservation` checks it.
//! Device interrupts land on the boot CPU only; the timer and shootdown IPI are per-CPU.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu;
use crate::scheduler::MAX_CPUS;

/// One variant per interrupt source this kernel counts; order matches `arch::idt`'s vector table.
/// Adding a device vector means adding a variant here and one `irq_took!` at its handler.
/// A variant nothing counts does not compile: `irq_took!` is the only reference, so `-D dead-code` refuses an unconstructed one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Source {
    /// Vector 0x20: shared by this CPU's LAPIC one-shot and every `apic::kick_cpu` IPI.
    Timer,
    /// Vector 0x21, xHCI MSI-X (or MSI).
    Xhci,
    /// Vector 0x22, virtio-net MSI-X.
    Net,
    /// Vector 0x23, virtio-sound MSI-X.
    Sound,
    /// Vector 0x24, the i8042's I/O APIC pin — both PS/2 lines.
    I8042,
    /// Vector 0x25, the remapping unit's fault event.
    DmaFault,
    /// Vector 0x26, HDA stream completion.
    Hda,
    /// Vector 0xFE, the TLB shootdown IPI.
    Tlb,
    /// Vector 0x02, and `sched::dump` is its only sender.
    Nmi,
    /// Vector 0xFF, the local APIC's spurious vector.
    /// A non-zero count on a machine that staged nothing is an interrupt-routing defect.
    Spurious,
}

impl Source {
    pub const COUNT: usize = 10;

    /// Order `tests/toyos.rs`'s `irq_census_conservation` parses back; must match variant order.
    pub const NAMES: [&'static str; Self::COUNT] =
        ["timer", "xhci", "net", "sound", "i8042", "dmafault", "hda", "tlb", "nmi", "spurious"];
}

/// One `u64` per source plus the total; `percpu::OFF_IRQ_COUNTS` is where the block starts.
pub const SLOTS: usize = 1 + Source::COUNT;

/// Index of the machine's own total inside a CPU's block.
pub const TOTAL: usize = 0;

/// The `gs:` displacement of slot `index` in this CPU's block.
pub const fn slot_offset(index: usize) -> u32 {
    percpu::OFF_IRQ_COUNTS + (index as u32) * 8
}

/// Records one delivery of `$source` as two lock-free `add`s to this CPU's own gs: slots.
/// A macro, not a function: the two offsets must be asm immediates, not const-generic values an optimiser could relax.
macro_rules! irq_took {
    ($source:ident) => {{
        // SAFETY: both slots are this CPU's own counter block per `arch::percpu`, and the caller is an interrupt handler, so `GS_BASE` already points at this CPU's `PerCpu`.
        unsafe {
            ::core::arch::asm!(
                "add qword ptr gs:[{total}], 1",
                "add qword ptr gs:[{source}], 1",
                total = const $crate::irq_census::slot_offset($crate::irq_census::TOTAL),
                source = const $crate::irq_census::slot_offset(
                    1 + $crate::irq_census::Source::$source as usize
                ),
                // no `nomem` because both instructions write; no `preserves_flags` because `add` clobbers flags.
                options(nostack),
            );
        }
    }};
}

pub(crate) use irq_took;

/// Each CPU's counter-array address; only the array is published, so a reader never touches the rest of the block the owning CPU writes through raw pointers.
static BLOCKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Publishes one CPU's counter block; called from `percpu::alloc_percpu` before that CPU runs.
pub(crate) fn publish(cpu_id: u32, block: *const AtomicU64) {
    let Some(slot) = BLOCKS.get(cpu_id as usize) else {
        return;
    };
    slot.store(block as u64, Ordering::Release);
}

/// One CPU's counters, or `None` if that CPU has never been built.
fn read(cpu: u32) -> Option<[u64; SLOTS]> {
    let base = BLOCKS.get(cpu as usize)?.load(Ordering::Acquire) as *const AtomicU64;
    if base.is_null() {
        return None;
    }
    let mut out = [0u64; SLOTS];
    for (i, slot) in out.iter_mut().enumerate() {
        // SAFETY: `base.add(i)` points at a live, in-bounds, single-writer counter word.
        *slot = unsafe { (*base.add(i)).load(Ordering::Relaxed) };
    }
    Some(out)
}

/// `name=value` for every source, in [`Source::NAMES`] order.
struct Fields<'a>(&'a [u64]);

impl fmt::Display for Fields<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, count) in Source::NAMES.iter().zip(self.0) {
            write!(f, " {name}={count}")?;
        }
        Ok(())
    }
}

/// One CPU's delivery count for `source`, or `None` if that CPU has never been built.
#[cfg(feature = "boot-actuators")]
pub fn deliveries(cpu: u32, source: Source) -> Option<u64> {
    read(cpu).map(|counts| counts[1 + source as usize])
}

/// Every CPU's total delivery count, or `None` if that CPU has never been built.
#[cfg(feature = "boot-actuators")]
pub fn deliveries_total(cpu: u32) -> Option<u64> {
    read(cpu).map(|counts| counts[TOTAL])
}

/// Logs one `irq: cpuN total=… <source>=…` line per online CPU; counts are cumulative since boot.
/// Allocates nothing, takes no lock, touches no device.
pub fn log_census() {
    for cpu in 0..crate::arch::smp::cpu_count() {
        let Some(counts) = read(cpu) else { continue };
        crate::log!(
            "irq: cpu{cpu} total={}{}",
            counts[TOTAL],
            Fields(&counts[TOTAL + 1..])
        );
    }
}
