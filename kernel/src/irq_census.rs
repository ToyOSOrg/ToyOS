//! Per-CPU, per-source interrupt delivery counts, kept in `PerCpu` for a lock-free `add`.
//! One word per delivery and no total beside it: a CPU's total is the sum of its sources,
//! because a second word another CPU reads between the two `add`s is a census that does not add up.
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
    /// Vector 0x20, this CPU's LAPIC one-shot: an expiry and nothing else.
    Timer,
    /// Vector 0xFC, `irqchip::kick_cpu`'s IPI.
    Kick,
    /// Vector 0x21, xHCI MSI-X (or MSI).
    Xhci,
    /// Vectors 0x28-0x2C: the MSI-X of a PCI function a process drives, and
    /// the lines of an ISA function one does. One count for all five: which
    /// claim an interrupt belonged to is the claim's own record, and the census
    /// is about this machine's interrupt routing.
    UserDev,
    /// Vector 0x24, the i8042's I/O APIC pin — both PS/2 lines.
    I8042,
    /// Vector 0x25, the remapping unit's fault event.
    DmaFault,
    /// Vector 0x26, HDA stream completion.
    Hda,
    /// Vector 0xFE, the TLB shootdown IPI.
    Tlb,
    /// Vector 0x02, which `crate::hardlockup` samples a CPU with.
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
        "timer", "kick", "xhci", "userdev", "i8042", "dmafault", "hda", "tlb", "nmi",
        "spurious", "unclaimed",
    ];
}

/// Every source's count summed, bar the NMI's.
fn sum_bar_nmi(counts: &[u64; Source::COUNT]) -> u64 {
    counts.iter().sum::<u64>() - counts[Source::Nmi as usize]
}

// Counted where each is taken, by the architecture's handlers
// (`arch::percpu::irq_took!`), into this CPU's own block.

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
fn read(cpu: u32) -> Option<[u64; Source::COUNT]> {
    let base = BLOCKS.get(cpu as usize)?.load(Ordering::Acquire) as *const AtomicU64;
    if base.is_null() {
        return None;
    }
    let mut out = [0u64; Source::COUNT];
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
pub fn deliveries(cpu: u32, source: Source) -> Option<u64> {
    read(cpu).map(|counts| counts[source as usize])
}

/// **One CPU's progress: every interrupt it has taken except the NMI.**
///
/// What `crate::hardlockup` compares one sample to the next, and the exclusion
/// is load-bearing: that sample arrives *as* an NMI and has already counted
/// itself by the time it reads this, so a total including it moves on every
/// sample and no CPU is ever stuck.
///
/// One published pointer and relaxed loads: no lock, so the CPU sealing a
/// record about a sibling can read this about it.
pub fn taken_by(cpu: u32) -> Option<u64> {
    read(cpu).map(|counts| sum_bar_nmi(&counts))
}

/// [`taken_by`] for the CPU asking, read straight off its own block — the one
/// form a CPU inside an NMI may use, since it needs neither the published
/// pointer array nor a bounds check on a `cpu_id` it is standing on.
pub fn taken_here() -> u64 {
    sum_bar_nmi(&percpu::irq_counts_here())
}

/// One `irq: cpuN <source>=…` line per online CPU; counts are cumulative since boot.
/// The machine's reading, so the machine's to take: `crate::census`'s, never one process's end.
/// Allocates nothing, takes no lock, touches no device.
pub fn census(say: &mut impl FnMut(fmt::Arguments<'_>)) {
    for cpu in 0..crate::smp::cpu_count() {
        let Some(counts) = read(cpu) else { continue };
        say(format_args!("irq: cpu{cpu}{}", Fields(&counts)));
    }
}
