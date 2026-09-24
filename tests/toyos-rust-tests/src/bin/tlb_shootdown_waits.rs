//! A shootdown returns when every other CPU has flushed — and the paths that
//! free memory are behind it.
//!
//! **Why this needs an actuator at all.** A correct wait and no wait whatsoever
//! measure the same zero on a machine where every CPU answers in microseconds,
//! so nothing a guest can do distinguishes them. `SYS_DEBUG` action 12 holds
//! each other CPU's acknowledgement back in turn — after the flush, so what is
//! staged is a slow answer and never an incorrect one — and answers with the
//! cheapest of those waits. So a wait that reaches only some of the target set
//! is a small number here rather than an invisible one, and the set's *width*
//! is assertable and not only its depth.
//!
//! **The precondition is the kernel's target set, and that set is every other
//! CPU on the machine.** Nothing this process does puts a CPU into it or takes
//! one out, so `SYS_CPU_COUNT` is the whole of the arrangement.
//!
//! **What is gated is the wait, not the harm.** A sibling reading through a
//! stale translation is not constructible under TCG — the correct outcome is a
//! fault that kills the observer, and a context switch writes CR3 and flushes
//! the whole TLB within milliseconds — so what is measured is the property that
//! closes the window: the free happens after the flush.

use toyos_abi::syscall::{self, MmapFlags, MmapProt, SYS_DEBUG};

/// Long enough to read off a clock through two syscalls, short enough that a
/// sweep of them is not a boot's worth of stalled CPU. The delay spins with
/// interrupts disabled on the target, which is why it is not larger.
const DELAY_NANOS: u64 = 20_000_000;

/// Half the delay. Every number compared against it is a lower bound on a spin
/// the kernel or this process performs, so it cannot come out short for
/// scheduling reasons — but the clock reads bracketing it are syscalls, and the
/// margin is there so a slow host cannot turn a pass into a fail either way.
const FLOOR_NANOS: u64 = DELAY_NANOS / 2;

/// How many measured `munmap`s must *all* return fast before that is the
/// verdict.
///
/// A target that is itself inside a shootdown publishes through a serve of its
/// own, which does not delay, so one fast return says nothing. A kernel that
/// does not wait returns fast every time.
const TRIALS: u32 = 5;

const PAGE_2M: usize = 2 * 1024 * 1024;

use toyos_abi::syscall::debug_action::{TLB_ACK_DELAY_ARM as ARM, TLB_ACK_DELAY_DISARM as DISARM};

fn debug(action: u64, arg: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") SYS_DEBUG,
            in("rsi") action,
            in("rdx") arg,
            in("r8") 0u64,
            in("r9") 0u64,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

/// Arm, and refuse to time anything until the kernel has just demonstrated the
/// wait it is about to be judged on.
///
/// The arming lapses after a window of its own, so a stage reached later than
/// that would otherwise time an unarmed machine and read its microseconds as a
/// missing wait.
fn armed(cpus: u32) {
    let least = debug(ARM, DELAY_NANOS);
    assert!(
        least >= FLOOR_NANOS,
        "with each of the {} other CPUs answering {DELAY_NANOS}ns late in turn, the cheapest \
         of those shootdowns cost the initiator {least}ns — it is not waiting for every other \
         CPU",
        cpus - 1,
    );
}

fn disarm() {
    debug(DISARM, 0);
}

fn timed(f: impl FnOnce()) -> u64 {
    let start = syscall::clock_nanos();
    f();
    syscall::clock_nanos() - start
}

fn map(size: usize) -> *mut u8 {
    let p = unsafe {
        syscall::mmap(
            core::ptr::null_mut(),
            size,
            MmapProt::READ | MmapProt::WRITE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!p.is_null(), "mmap failed");
    p
}

fn main() {
    // The set the kernel computes, asked of the kernel. A machine with one CPU
    // takes `shootdown`'s local-flush return, which issues no IPI and waits for
    // nobody, so there is no wait to measure and a fast return is correct.
    let cpus = syscall::cpu_count();
    assert!(
        cpus > 1,
        "this guest has {cpus} CPU, so a shootdown has nobody to wait for and takes the \
         local-flush return — the stage measures a wait and this machine has none",
    );

    // 1. `munmap`, which is the syscall the stage exists for: the pages go back
    //    to the PMM behind the flush.
    let mut judged = false;
    for trial in 1..=TRIALS {
        let region = map(PAGE_2M);
        armed(cpus);
        let elapsed = timed(|| {
            unsafe { syscall::munmap(region, PAGE_2M) }.expect("munmap");
        });
        if elapsed >= FLOOR_NANOS {
            judged = true;
            break;
        }
        println!("trial {trial}: munmap returned in {elapsed}ns, under the {FLOOR_NANOS}ns floor");
    }
    assert!(
        judged,
        "every one of {TRIALS} munmaps returned in under {FLOOR_NANOS}ns with all {} other \
         CPUs answering {DELAY_NANOS}ns late, each one measured being waited for immediately \
         before — it freed the pages without waiting for the flush",
        cpus - 1,
    );

    // 2. A fixed mapping placed over a range, which is a *remap* rather than a
    //    free: the address keeps its meaning and changes what it names, so a
    //    sibling holding the old translation writes into the wrong physical
    //    page with nothing ever faulting.
    let placed = map(PAGE_2M);
    armed(cpus);
    let elapsed = timed(|| {
        let p = unsafe {
            syscall::mmap(
                placed,
                PAGE_2M,
                MmapProt::READ | MmapProt::WRITE,
                MmapFlags::ANONYMOUS | MmapFlags::PRIVATE | MmapFlags::FIXED,
            )
        };
        assert_eq!(p, placed, "MAP_FIXED did not honour the address");
    });
    assert!(
        elapsed >= FLOOR_NANOS,
        "a fixed mmap returned in {elapsed}ns with every other CPU answering {DELAY_NANOS}ns \
         late — it replaced the mapping without waiting for the flush",
    );
    unsafe { syscall::munmap(placed, PAGE_2M) }.expect("munmap the fixed mapping");

    disarm();

    // 3. And the delay is what produced every number above, not the machine:
    //    disarmed, the same operation is back to microseconds. Without this the
    //    assertions above would still pass on a kernel that happened to be slow
    //    for some other reason.
    let quiet = map(PAGE_2M);
    let elapsed = timed(|| {
        unsafe { syscall::munmap(quiet, PAGE_2M) }.expect("munmap");
    });
    assert!(
        elapsed < FLOOR_NANOS,
        "munmap still took {elapsed}ns with the delay disarmed, so the numbers above \
         measured something other than the wait",
    );

    println!("a shootdown waits for every other CPU, and munmap and a fixed mmap wait for it");
}
