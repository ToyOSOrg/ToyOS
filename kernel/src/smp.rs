//! The machine's CPUs: the one [`ROSTER`] each architecture's bring-up
//! (`arch::smp`) commits its APs to, what the rest of the kernel asks of it,
//! and what starting an AP takes on either architecture.

use core::mem::size_of;

use alloc::boxed::Box;

use crate::smp_roster::{Roster, MAX_CPUS};

pub static ROSTER: Roster = Roster::new();

const _: () = assert!(MAX_CPUS == crate::scheduler::MAX_CPUS);

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
