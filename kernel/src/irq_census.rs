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
    /// Vector 0x20: shared by this CPU's LAPIC one-shot and every `irqchip::kick_cpu` IPI.
    Timer,
    /// Vector 0x21, xHCI MSI-X (or MSI).
    Xhci,
    /// Vectors 0x28-0x2B, the MSI-X of a PCI function a process drives. One
    /// count for all four: which claim a message belonged to is the claim's own
    /// record, and the census is about this machine's interrupt routing.
    UserDev,
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
    /// Every vector no `idt_vectors!` row claims — `arch::trap::unclaimed`.
    /// A non-zero count a boot staged nothing for is a routing defect the gate kept off `#DF`.
    Unclaimed,
}

impl Source {
    pub const COUNT: usize = 11;

    /// Order `tests/toyos.rs`'s `irq_census_conservation` parses back; must match variant order.
    pub const NAMES: [&'static str; Self::COUNT] = [
        "timer", "xhci", "userdev", "sound", "i8042", "dmafault", "hda", "tlb", "nmi",
        "spurious", "unclaimed",
    ];
}

/// One `u64` per source plus the total, in each CPU's own per-CPU block.
pub const SLOTS: usize = 1 + Source::COUNT;

/// Index of the machine's own total inside a CPU's block.
pub const TOTAL: usize = 0;

/// Counted where each is taken, by the architecture's handlers
/// (`arch::percpu::irq_took!`), into this CPU's own block.

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

/// **One CPU's progress: every interrupt it has taken except the NMI.**
///
/// What `crate::hardlockup` compares one sample to the next, and the exclusion
/// is the whole of why this is not [`deliveries_total`]: that sample arrives
/// *as* an NMI and has already counted itself by the time it reads this, so a
/// total including it moves on every sample and no CPU is ever stuck.
///
/// One published pointer and two relaxed loads: no lock, so the CPU sealing a
/// record about a sibling can read this about it.
pub fn taken_by(cpu: u32) -> Option<u64> {
    read(cpu).map(|counts| counts[TOTAL].saturating_sub(counts[1 + Source::Nmi as usize]))
}

/// [`taken_by`] for the CPU asking, read straight off its own block — the one
/// form a CPU inside an NMI may use, since it needs neither the published
/// pointer array nor a bounds check on a `cpu_id` it is standing on.
pub fn taken_here() -> u64 {
    let (total, nmis) = percpu::irq_counts_here(TOTAL, 1 + Source::Nmi as usize);
    total.saturating_sub(nmis)
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
