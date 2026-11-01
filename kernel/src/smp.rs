//! The machine's CPUs: the one [`ROSTER`] each architecture's bring-up
//! (`arch::smp`) commits its APs to, what the rest of the kernel asks of it,
//! and what starting an AP takes on either architecture.

use core::mem::size_of;
use core::sync::atomic::{AtomicPtr, Ordering};

use alloc::boxed::Box;
use alloc::vec::Vec;
use kernel::sched::topology::{CpuKeys, Level, Topology};

use crate::log;

use crate::smp_roster::Roster;

pub static ROSTER: Roster = Roster::new();

/// Size every per-CPU table for the `capacity` CPUs the MADT enables, while
/// the boot CPU runs alone and before its own per-CPU block exists: once, and
/// never freed, so no fatal path allocates.
///
/// Bounded by a thread's CPU mask, which names
/// [`kernel::sched::affinity::MAX_CPUS`]: a machine past it is refused by
/// name.
pub fn size(capacity: usize) {
    assert!(capacity > 0, "smp: the MADT enables no processor, the boot CPU's own among them");
    assert!(
        capacity <= kernel::sched::affinity::MAX_CPUS,
        "smp: the MADT enables {capacity} processors and a CPU mask names {}",
        kernel::sched::affinity::MAX_CPUS,
    );
    ROSTER.size(capacity);
    crate::cpu_local::size(capacity);
    crate::arch::size(capacity);
    crate::counters::size(capacity);
    crate::log::size(capacity);
    crate::trace::size(capacity);
    log!("smp: tables sized for the {capacity} processors the MADT enables");
}

/// The CPUs the firmware names, online or not: what every per-CPU table and
/// log cursor is sized by. 1 before [`size`].
pub fn capacity() -> u32 {
    ROSTER.capacity().max(1) as u32
}

/// The CPUs committed so far, dense ids `0..cpu_count()`.
pub fn cpu_count() -> u32 {
    ROSTER.count()
}

/// `cpu`'s hardware id — its LAPIC id on x86-64, its packed MPIDR affinity on
/// AArch64; panics if `cpu` is not online.
pub fn hardware_id(cpu: u32) -> u32 {
    assert!(cpu < cpu_count(), "smp: cpu{cpu} is not online");
    ROSTER.hardware_id(cpu)
}

/// True once a shootdown must wait for siblings; the word the APs are released by.
pub fn answering() -> bool {
    ROSTER.answering()
}

/// Release the APs into the scheduler and, by the same store, start answering their shootdowns.
pub fn set_ready() {
    ROSTER.release();
}

/// Whether [`set_ready`] has run: the machine's own word for "the scheduler is
/// what runs now". `kernel_main` calls it immediately before
/// `scheduler::enter_idle_loop`, so a `false` here means no task and no
/// kernel thread can make progress, and the panic path waits for none of them.
pub fn is_ready() -> bool {
    ROSTER.released()
}

static TOPOLOGY: AtomicPtr<Topology> = AtomicPtr::new(core::ptr::null_mut());

/// Install the cache tree, from each online CPU's keys in dense-id order: once,
/// by bring-up, after its last commit. Keys that are not one tree stop the
/// boot by name rather than schedule on a guess.
pub fn set_topology(keys: Vec<CpuKeys>) {
    assert_eq!(keys.len(), cpu_count() as usize, "smp: a topology key set for every online cpu and no other");
    for (cpu, k) in keys.iter().enumerate() {
        log!("topo: cpu{cpu} core={:#x} llc={:#x} pkg={:#x}", k.core, k.llc, k.package);
    }
    let topology = Topology::new(keys).unwrap_or_else(|e| panic!("smp: the CPUs' keys are no tree: {e:?}"));
    log!(
        "topo: {} cpus in {} cores, {} llcs and {} packages",
        topology.cpus(),
        topology.domains(Level::Core),
        topology.domains(Level::Llc),
        topology.domains(Level::Package),
    );
    let was = TOPOLOGY.swap(Box::into_raw(Box::new(topology)), Ordering::Release);
    assert!(was.is_null(), "smp: the topology is installed once");
}

/// The tree [`set_topology`] installed.
pub fn topology() -> &'static Topology {
    let ptr = TOPOLOGY.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "smp: the topology was read before bring-up installed it");
    // SAFETY: installed once from a leaked `Box` and never freed.
    unsafe { &*ptr }
}

/// The actuator staging a non-last AP that never starts; `false` without `boot-actuators`.
pub fn skip_startup(id: u32) -> bool {
    crate::actuator::smp_skip_ap() && id == 2
}

/// The top of a stack an AP runs on from its entry until its idle loop, which
/// has a stack of its own. Never freed: nothing says when the AP has left it.
pub fn bringup_stack() -> u64 {
    #[repr(C, align(16))]
    struct Stack([u8; 64 * 1024]);
    let stack = Box::leak(Box::<Stack>::new_uninit());
    stack.as_ptr() as u64 + size_of::<Stack>() as u64
}
