//! Every CPU's kernel-wide record: one per CPU the firmware names, each its
//! own allocation, made by [`crate::smp::size`] before the boot CPU's per-CPU
//! block exists and never freed. Each module keeps its part here as a `Local`
//! of its own; its owner writes it and any CPU may read it.
//!
//! A record is aligned past a cache line and its neighbour's prefetch pair, so
//! no two CPUs' records share a line.

use crate::cpu_slice::CpuSlice;

#[repr(align(128))]
pub struct CpuLocal {
    pub arch: crate::arch::CpuLocal,
    pub sched: crate::sched::driver::Local,
    pub hardlockup: crate::hardlockup::Local,
    pub deadline: crate::deadline::Local,
    pub hw: crate::hw::Local,
    pub irq_census: crate::irq_census::Local,
    pub counters: crate::counters::Local,
    pub idle: crate::idle::Local,
    #[cfg(feature = "mask-windows")]
    pub windows: crate::windows::Local,
}

static TABLE: CpuSlice<&'static CpuLocal> = CpuSlice::new();

pub fn size(capacity: usize) {
    TABLE.size(capacity, |_| alloc::boxed::Box::leak(alloc::boxed::Box::new(CpuLocal {
        arch: crate::arch::CpuLocal::new(),
        sched: crate::sched::driver::Local::new(),
        hardlockup: crate::hardlockup::Local::new(),
        deadline: crate::deadline::Local::new(),
        hw: crate::hw::Local::new(),
        irq_census: crate::irq_census::Local::new(),
        counters: crate::counters::Local::new(),
        idle: crate::idle::Local::new(),
        #[cfg(feature = "mask-windows")]
        windows: crate::windows::Local::new(),
    })));
}

/// `cpu`'s record; a CPU past the table, or a read before it is sized, is a
/// kernel bug.
pub fn of(cpu: u32) -> &'static CpuLocal {
    TABLE[cpu as usize]
}

/// The record of the CPU this runs on.
pub fn here() -> &'static CpuLocal {
    of(crate::arch::percpu::cpu_id())
}

/// `cpu`'s record, or `None` before the table is sized: for a crash report,
/// which may run before it is.
pub fn get(cpu: u32) -> Option<&'static CpuLocal> {
    TABLE.get(cpu as usize).copied()
}
