//! `KernelHw` — the kernel's side of the scheduler-core hardware boundary: the
//! one-shot timer, the kick, the halt and the context switch. Nothing here
//! makes a scheduling decision; the simulator that exercises the scheduler
//! core replaces this and nothing else.
//!
//! The halt and the switch are the architecture's (`crate::arch::hw`);
//! everything else is here, once, reaching the machine through
//! `crate::arch::irqchip`.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use kernel::sched::cpu::SleepToken;
use kernel::sched::task::QUANTUM_NS;
use kernel::sched::hw::{CpuId, Kicker, Machine, Nanos, TraceEvent};

use crate::arch::{irqchip, percpu};
use crate::sched::payload::KernelCtx;
use crate::time::{Duration, Floor};

/// The one instance; zero-sized, holds no per-CPU state.
pub static HW: KernelHw = KernelHw;

pub struct KernelHw;

/// The scheduler clock, in raw nanoseconds.
pub fn now_ns() -> u64 {
    HW.now().0
}

/// The shortest one-shot either timer is armed for, whoever asks: a count
/// that expires before the interrupt it schedules retires re-fires on the
/// return from it, and a context held with interrupts open makes no progress.
pub(crate) const MIN_ONE_SHOT: Floor =
    Floor::policy(Duration::from_micros(10), "above an interrupt's entry and return");

impl Kicker for KernelHw {
    fn kick(&self, target: CpuId) {
        if self.runs_on(target) {
            crate::preempt::set_need_resched();
        } else {
            irqchip::kick_cpu(target.0);
        }
    }

    fn runs_on(&self, cpu: CpuId) -> bool {
        cpu.0 == percpu::cpu_id()
    }
}

impl Machine for KernelHw {
    fn now(&self) -> Nanos {
        Nanos(crate::clock::nanos_since_boot())
    }

    /// The absolute deadline as the one-shot's span from now; one already
    /// past becomes [`MIN_ONE_SHOT`] rather than an interrupt at once.
    fn set_timer(&self, deadline: Nanos) {
        irqchip::arm_one_shot(deadline.0.saturating_sub(self.now().0));
    }

    fn stop_timer(&self) {
        irqchip::stop_timer();
    }

    fn idle_plan(&self, deadline: Option<Nanos>, now: Nanos) -> kernel::sched::idle::Plan {
        crate::idle::plan(deadline, now)
    }

    fn halt(&self) {
        crate::arch::hw::halt();
    }

    /// A kick is how a remote CPU's `need_resched` gets set — there is no way to write it directly.
    fn need_resched(&self, cpu: CpuId) {
        self.kick(cpu);
    }

    fn trace(&self, ev: TraceEvent) {
        crate::trace::record(ev);
    }

    /// Waits in the state `crate::idle` chooses. Diagnostic builds arm a
    /// periodic wake first, so a quiescent CPU still reports.
    fn idle_wait(&self, token: SleepToken) {
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::diag_tick() {
            irqchip::arm_within(DIAG_TICK_NS);
        }
        #[cfg(feature = "mask-windows")]
        crate::windows::halting();
        crate::idle::wait(token.idle());
        #[cfg(feature = "mask-windows")]
        crate::windows::woken();
        // **A CPU that is executing has a one-shot armed, and this is where
        // that becomes true again.** `TimerPlan::Stop` left this one at zero
        // before the halt above and only a pass reaching `apply_timer` arms
        // another, so a CPU woken by a kick or a device — never by its own
        // timer, which is stopped — and then held in the kernel takes no timer
        // interrupt at all. `crate::deadline`'s poll and `crate::hardlockup`'s
        // sample both rest on some CPU taking one. Arming earlier than the
        // scheduler planned is a spurious pass and never a missed deadline,
        // and the next pass replaces it either way.
        //
        // Asked, because it is only those two that need it: a boot under no
        // bound pays a timer read and two writes per wake for nothing.
        if crate::deadline::armed() {
            irqchip::arm_within(QUANTUM_NS);
        }
    }
}

/// Longest sleep on a `diag-tick` build.
#[cfg(feature = "boot-actuators")]
const DIAG_TICK_NS: u64 = 100_000_000;

/// Which context a CPU last switched onto; read by [`report_contexts`] on crash, since a
/// sibling's real `CpuSched` is `!Sync` and unreadable directly.
pub struct Local {
    running_ctx: AtomicU64,
}

impl Local {
    pub fn new() -> Self {
        Self { running_ctx: AtomicU64::new(0) }
    }
}

/// This CPU is about to stand on `ctx`: `Hw::switch`'s last word before the stack moves.
pub(crate) fn note_running(ctx: *const KernelCtx) {
    crate::cpu_local::here().hw.running_ctx.store(ctx as u64, Relaxed);
}

/// Prints which CPU is standing on which context and stack, on every kernel crash.
///
/// `subject` (`None` for this CPU's own) is the context flagged as "the same".
///
/// Allocates, locks or formats nothing but integers, since a crash may already hold any lock this
/// could try to take.
pub fn report_contexts(sp: u64, subject: Option<u64>) {
    let me = percpu::cpu_id() as usize;
    let running = |cpu: usize| crate::cpu_local::get(cpu as u32).map_or(0, |local| local.hw.running_ctx.load(Relaxed));
    let mine = running(me);
    let subject = subject.unwrap_or(mine);
    crate::log!("  Contexts: cpu{me} crashed at sp={sp:#018x}, asking about ctx {subject:#x}");
    for cpu in 0..crate::smp::cpu_count() as usize {
        let held = running(cpu);
        if !crate::mm::is_kernel_addr(held) || !held.is_multiple_of(8) {
            crate::log!("  cpu{cpu} is on ctx {held:#x} (never switched, or not a context)");
            continue;
        }
        // SAFETY: `held` is a pointer this kernel's own `Hw::switch` stored, into the boxed, always-mapped direct map.
        let ctx = unsafe { &*(held as *const KernelCtx) };
        let top = ctx.kernel_stack_top;
        let same = held == subject && cpu != me;
        // `top != 0` excludes idle contexts, whose stack top is zero by construction — the
        // containment test below never fires for one; that is a gap in this report, not a bug.
        let on_its_stack = cpu != me
            && top != 0
            && sp <= top
            && sp > top.wrapping_sub(crate::process::KERNEL_STACK_SIZE as u64);
        // idle's `kernel_stack_top` is zero by construction; rendering it as a task would misread as corruption.
        match ctx.runs.id() {
            None => crate::log!(
                "  cpu{cpu} is on ctx {held:#x} (its idle context) stack_top={top:#018x} \
                 saved_sp={:#018x}{}{}",
                ctx.sp,
                if same { "  <== THE SAME CONTEXT" } else { "" },
                if top == 0 { "" } else { "  <== AN IDLE CONTEXT'S STACK TOP IS ZERO BY CONSTRUCTION" },
            ),
            Some(id) => crate::log!(
                "  cpu{cpu} is on ctx {held:#x} pid={} tid={} stack_top={top:#018x} \
                 saved_sp={:#018x}{}{}",
                id.0.raw(),
                id.1.raw(),
                ctx.sp,
                if same { "  <== THE SAME CONTEXT" } else { "" },
                if on_its_stack { "  <== AND THIS CRASH IS ON THAT STACK" } else { "" },
            ),
        }
    }
    crate::mm::report_on_crash();
}
