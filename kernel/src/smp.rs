//! The machine's CPUs: the one [`ROSTER`] each architecture's bring-up
//! (`arch::smp`) commits its APs to, what the rest of the kernel asks of it,
//! and what starting an AP takes on either architecture.

use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use alloc::boxed::Box;

use crate::smp_roster::{Attempt, Roster, MAX_CPUS};

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

/// The hardware id the AP being started reads as its own, said with its echo.
static ECHOED_ID: AtomicU32 = AtomicU32::new(0);

/// The started AP's half of the handshake: the hardware id it reads as its
/// own, then [`Roster::echo`].
pub fn echo(token: u32) {
    ECHOED_ID.store(crate::arch::cpu::hardware_id(), Ordering::Relaxed);
    ROSTER.echo(token);
}

/// Commit the AP that echoed `at` under `hardware_id`, the id its roster slot
/// and every IPI name it by, which must be the one it reads as its own: its
/// fatal paths and the console lock name it by that read. Refused here, on the
/// boot CPU, because before the release an AP's own panic stops no other CPU.
pub fn commit(at: Attempt, hardware_id: u32) {
    // Ordered by the echo, whose acquire `Roster::await_echo` took.
    let read = ECHOED_ID.load(Ordering::Relaxed);
    assert_eq!(
        read,
        hardware_id,
        "smp: cpu{} reads its own hardware id as {read:#x}, and its roster slot and every IPI name it {hardware_id:#x}",
        at.id()
    );
    ROSTER.commit(at, hardware_id);
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
