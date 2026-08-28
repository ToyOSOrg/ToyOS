//! `KernelHw` — the kernel's side of the scheduler-core hardware boundary.
//!
//! Everything here is x2APIC, TSC or a single instruction; nothing here
//! makes scheduling decisions. The simulator that exercises the scheduler
//! core replaces this file and nothing else.

use core::arch::asm;

use toyos_sched::cpu::SleepToken;
use toyos_sched::cpu::RunToken;
use toyos_sched::hw::{CpuId, Hw, Kicker, Machine, Nanos, TraceEvent};
use toyos_sched::task::{TaskAccounting, TaskKey};

use crate::arch::{apic, cpu, percpu};
use crate::sched::driver::context_switch;
use crate::sched::payload::{KernelCtx, KernelPayload};

/// The one instance; zero-sized, holds no per-CPU state.
pub static HW: KernelHw = KernelHw;

pub struct KernelHw;

/// The scheduler clock, in raw nanoseconds.
pub fn now_ns() -> u64 {
    HW.now().0
}

/// RAII interrupt gate: restores the caller's `IF` rather than setting it, so nesting inside an already-closed region is safe.
#[must_use = "the interrupt gate closes when the guard drops"]
pub struct IrqGuard {
    rflags: u64,
}

impl IrqGuard {
    pub fn close() -> Self {
        let rflags: u64;
        // SAFETY: touches only RFLAGS and one pushed-and-popped stack slot; `cli` cannot fail in
        // Ring 0 — reading the flags and closing them must be one uninterruptible sequence, which
        // two safe calls could not guarantee.
        unsafe {
            asm!("pushfq", "pop {}", "cli", out(reg) rflags, options(nomem));
        }
        Self { rflags }
    }
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        // SAFETY: `rflags` is the word this guard's own `close` read out of `RFLAGS` on this CPU —
        // restoring it is not `sti`, and `arch::cpu` has no safe primitive for that.
        unsafe {
            asm!("push {}", "popfq", in(reg) self.rflags, options(nomem));
        }
    }
}

impl Kicker for KernelHw {
    fn kick(&self, target: CpuId) {
        apic::kick_cpu(target.0);
    }
}

impl Machine for KernelHw {
    type IrqGuard = IrqGuard;

    fn now(&self) -> Nanos {
        Nanos(crate::clock::nanos_since_boot())
    }

    /// Converts the trait's absolute deadline to the one-shot timer's relative count; a deadline
    /// already past saturates to zero rather than firing immediately, which would spin the Ring 0
    /// stub in a reload loop.
    fn set_timer(&self, deadline: Nanos) {
        apic::arm_one_shot(deadline.0.saturating_sub(self.now().0));
    }

    fn stop_timer(&self) {
        apic::stop_timer();
    }

    fn irq_guard(&self) -> IrqGuard {
        IrqGuard::close()
    }

    fn halt(&self) {
        // SAFETY: `sti; hlt` is the atomic enable-and-wait pair — a wake landing between the two is not lost.
        unsafe { asm!("sti; hlt", options(nomem, nostack)); }
    }

    /// A kick IPI is how a remote CPU's `need_resched` gets set — there is no way to write it directly.
    fn need_resched(&self, cpu: CpuId) {
        if cpu.0 == percpu::cpu_id() {
            crate::preempt::set_need_resched();
        } else {
            self.kick(cpu);
        }
    }

    fn trace(&self, ev: TraceEvent) {
        crate::trace::record(ev);
    }

    /// Diagnostic builds arm a periodic wake before halting so a quiescent CPU still reports.
    fn idle_wait(&self, token: SleepToken) {
        let _consumed = token;
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::diag_tick() {
            apic::arm_within(DIAG_TICK_NS);
        }
        self.halt();
    }
}

/// Longest sleep on a `diag-tick` build; kept under `heartbeat`'s reporting period so a healthy CPU reports on every line.
#[cfg(feature = "boot-actuators")]
const DIAG_TICK_NS: u64 = 100_000_000;

/// Which context each CPU last switched onto; read by [`report_contexts`] on crash, since a
/// sibling's real `CpuSched` is `!Sync` and unreadable directly.
static RUNNING_CTX: [core::sync::atomic::AtomicU64; crate::sched::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::sched::MAX_CPUS];

/// Prints which CPU is standing on which context and stack, on every kernel crash.
///
/// `subject` (`None` for this CPU's own) is the context flagged as "the same".
///
/// Allocates, locks or formats nothing but integers, since a crash may already hold any lock this
/// could try to take.
pub fn report_contexts(rsp: u64, subject: Option<u64>) {
    let me = percpu::cpu_id() as usize;
    let count = (crate::arch::smp::cpu_count() as usize).min(crate::sched::MAX_CPUS);
    let mine = RUNNING_CTX
        .get(me)
        .map_or(0, |slot| slot.load(core::sync::atomic::Ordering::Relaxed));
    let subject = subject.unwrap_or(mine);
    crate::log!("  Contexts: cpu{me} crashed at rsp={rsp:#018x}, asking about ctx {subject:#x}");
    for (cpu, slot) in RUNNING_CTX.iter().enumerate().take(count) {
        let held = slot.load(core::sync::atomic::Ordering::Relaxed);
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
            && rsp <= top
            && rsp > top.wrapping_sub(crate::process::KERNEL_STACK_SIZE as u64);
        // idle's `kernel_stack_top` is zero by construction; rendering it as a task would misread as corruption.
        match ctx.id {
            None => crate::log!(
                "  cpu{cpu} is on ctx {held:#x} (its idle context) stack_top={top:#018x} \
                 saved_rsp={:#018x}{}{}",
                ctx.rsp,
                if same { "  <== THE SAME CONTEXT" } else { "" },
                if top == 0 { "" } else { "  <== AN IDLE CONTEXT'S STACK TOP IS ZERO BY CONSTRUCTION" },
            ),
            Some(id) => crate::log!(
                "  cpu{cpu} is on ctx {held:#x} pid={} tid={} stack_top={top:#018x} \
                 saved_rsp={:#018x}{}{}",
                id.0.raw(),
                id.1.raw(),
                ctx.rsp,
                if same { "  <== THE SAME CONTEXT" } else { "" },
                if on_its_stack { "  <== AND THIS CRASH IS ON THAT STACK" } else { "" },
            ),
        }
    }
    if let Some((used, of)) = crate::sched::driver::stack_high_water() {
        crate::log!("  Task kernel stacks: deepest {used} of {of} bytes");
    }
    if let Some((sweeps, records, overflowed)) = crate::mm::sweep_stats() {
        crate::log!(
            "  Heap sweeps: {sweeps} run, {records} live bands on the last walk{}",
            if overflowed { ", and the page table filled — the walk is incomplete" } else { "" },
        );
    }
}

/// Panics before the wild `ret` would restore register state that makes the failure unnameable.
#[cold]
#[inline(never)]
fn switch_frame_is_wrong(ctx: &KernelCtx, token: &RunToken<KernelPayload>) -> ! {
    let rsp = ctx.rsp;
    let (pid, tid) = ctx.id.map_or((u32::MAX, u32::MAX), |id| (id.0.raw(), id.1.raw()));
    crate::log!(
        "CONTEXT SWITCH ONTO A FRAME THAT IS NOT ONE: cpu={} pid={pid} tid={tid} \
         rsp={:#018x} top={:#018x} (top-rsp={}, and 64 is the entry frame, so a \
         context never saved) preempt={} fs_base={:#018x} incoming key={:?} \
         outgoing key={:?}",
        percpu::cpu_id(),
        rsp,
        ctx.kernel_stack_top,
        ctx.kernel_stack_top.wrapping_sub(rsp) as i64,
        ctx.preempt,
        ctx.fs_base,
        token.incoming().map(|k| k.0),
        token.outgoing().map(|k| k.0),
    );
    report_contexts(rsp, Some(ctx as *const KernelCtx as u64));
    if crate::mm::is_kernel_addr(rsp) && rsp.is_multiple_of(8) {
        const NAMES: [&str; 8] =
            ["r15", "r14", "r13", "r12", "rbx", "rbp", "rflags", "ret"];
        for (i, name) in NAMES.iter().enumerate() {
            let addr = rsp + (i as u64) * 8;
            // SAFETY: inside the incoming task's own kernel stack, bounded by `kernel_stack_top` and `KERNEL_STACK_SIZE`.
            let word = unsafe { core::ptr::read_volatile(addr as *const u64) };
            crate::log!("  [{addr:#x}] {name:>6} = {word:#018x}");
        }
    }
    panic!(
        "context_switch: the frame about to be restored is not one — its rsp is not a kernel \
         address, or its return slot is not kernel text, or (under `stack-witness`) it is not \
         inside the stack this context's own `kernel_stack_top` names"
    );
}

/// Returns the validated `rsp` — callers must use this value; re-reading `ctx.rsp` here would be a second, unguarded load after `cr3.activate()`'s memory clobber.
#[inline]
#[must_use]
fn check_switch_frame(ctx: &KernelCtx, token: &RunToken<KernelPayload>) -> u64 {
    let rsp = ctx.rsp;
    if !crate::mm::is_kernel_addr(rsp) || !rsp.is_multiple_of(8) {
        switch_frame_is_wrong(ctx, token);
    }
    // `rsp` must lie inside the stack this context's own `kernel_stack_top` names, not merely be a kernel address.
    // The idle context is not exempt: its stack is knowable only here, on the CPU it belongs to.
    #[cfg(feature = "stack-witness")]
    {
        let top = match ctx.id {
            Some(_) => ctx.kernel_stack_top,
            None => percpu::idle_stack_top(),
        };
        if rsp > top || rsp <= top - crate::process::KERNEL_STACK_SIZE as u64 {
            switch_frame_is_wrong(ctx, token);
        }
    }
    // SAFETY: `rsp` is eight-aligned inside the incoming stack at minimum depth, so the return slot is mapped.
    let ret = unsafe { core::ptr::read_volatile((rsp + 56) as *const u64) };
    if !crate::mm::is_kernel_addr(ret) {
        switch_frame_is_wrong(ctx, token);
    }
    #[cfg(feature = "switch-witness")]
    switch_witness_capture(ctx, token, rsp);
    rsp
}

/// The frame and pointer [`switch_witness_verify`] compares against, captured at the moment [`check_switch_frame`] validated them.
#[cfg(feature = "switch-witness")]
struct SwitchShadow {
    rsp: u64,
    words: [u64; 8],
    ctx: *const KernelCtx,
    save: u64,
    top: u64,
    incoming: u64,
    outgoing: u64,
}

#[cfg(feature = "switch-witness")]
struct SwitchShadowSlot(core::cell::UnsafeCell<SwitchShadow>);

// SAFETY: each slot is touched only by its own CPU, which cannot be preempted across the write-then-read window.
#[cfg(feature = "switch-witness")]
unsafe impl Sync for SwitchShadowSlot {}

#[cfg(feature = "switch-witness")]
static SWITCH_SHADOW: [SwitchShadowSlot; crate::sched::MAX_CPUS] = [const {
    SwitchShadowSlot(core::cell::UnsafeCell::new(SwitchShadow {
        rsp: 0,
        words: [0; 8],
        ctx: core::ptr::null(),
        save: 0,
        top: 0,
        incoming: u64::MAX,
        outgoing: u64::MAX,
    }))
}; crate::sched::MAX_CPUS];

#[cfg(feature = "switch-witness")]
fn switch_witness_capture(ctx: &KernelCtx, token: &RunToken<KernelPayload>, rsp: u64) {
    // SAFETY: this CPU's own slot; see the `Sync` justification above.
    let shadow = unsafe { &mut *SWITCH_SHADOW[percpu::cpu_id() as usize].0.get() };
    shadow.rsp = rsp;
    shadow.ctx = ctx as *const KernelCtx;
    shadow.save = token.save_ptr() as u64;
    shadow.top = match ctx.id {
        Some(_) => ctx.kernel_stack_top,
        None => percpu::idle_stack_top(),
    };
    shadow.incoming = token.incoming().map_or(u64::MAX, |k| k.0);
    shadow.outgoing = token.outgoing().map_or(u64::MAX, |k| k.0);
    for (i, word) in shadow.words.iter_mut().enumerate() {
        // SAFETY: `check_switch_frame` already validated this range as the incoming stack's own frame.
        *word = unsafe { core::ptr::read_volatile((rsp + (i as u64) * 8) as *const u64) };
    }
}

/// Compares the frame about to be popped against the one [`check_switch_frame`] validated.
/// # Safety
/// Must run from [`crate::sched::driver::context_switch`], with `rsp` equal to the live stack
/// pointer and this CPU's shadow already filled by [`switch_witness_capture`].
#[cfg(feature = "switch-witness")]
pub(crate) unsafe extern "C" fn switch_witness_verify(rsp: u64) {
    // SAFETY: this CPU's own slot; see the `Sync` justification above.
    let shadow = unsafe { &*SWITCH_SHADOW[percpu::cpu_id() as usize].0.get() };
    let mut now = [0u64; 8];
    for (i, word) in now.iter_mut().enumerate() {
        // SAFETY: within the kernel stack this CPU is standing on.
        *word = unsafe { core::ptr::read_volatile((rsp + (i as u64) * 8) as *const u64) };
    }
    // SAFETY: `shadow.ctx` is a live `KernelCtx` from this kernel's own pass.
    let field = unsafe { core::ptr::read_volatile(&raw const (*shadow.ctx).rsp) };
    if rsp == shadow.rsp && field == shadow.rsp && now == shadow.words {
        return;
    }
    switch_window_is_wrong(rsp, field, &now, shadow);
}

/// A frame word — or the pointer to it — changed between the check and the pop.
#[cfg(feature = "switch-witness")]
#[cold]
#[inline(never)]
fn switch_window_is_wrong(rsp: u64, field: u64, now: &[u64; 8], shadow: &SwitchShadow) -> ! {
    const NAMES: [&str; 8] = ["r15", "r14", "r13", "r12", "rbx", "rbp", "rflags", "ret"];
    crate::log!(
        "SWITCH WINDOW: cpu{} the frame is not the one that was checked — checked \
         rsp={:#018x}, standing on rsp={:#018x} ({}), the incoming ctx {:#x} now says \
         {field:#018x} ({}), outgoing ctx (rdi) {:#018x}; incoming key={} outgoing key={}; \
         the incoming stack is [{:#018x}, {:#018x})",
        percpu::cpu_id(),
        shadow.rsp,
        rsp,
        if rsp == shadow.rsp { "THE SAME" } else { "MOVED" },
        shadow.ctx as u64,
        if field == shadow.rsp { "unchanged" } else { "CHANGED SINCE THE CHECK" },
        shadow.save,
        shadow.incoming,
        shadow.outgoing,
        shadow.top.wrapping_sub(crate::process::KERNEL_STACK_SIZE as u64),
        shadow.top,
    );
    for (i, name) in NAMES.iter().enumerate() {
        let checked = shadow.rsp + (i as u64) * 8;
        let standing = rsp + (i as u64) * 8;
        // SAFETY: both addresses are inside a stack this CPU is executing on or is about to.
        let there_now = unsafe { core::ptr::read_volatile(checked as *const u64) };
        crate::log!(
            "  {name:>6}: [{checked:#x}] was {:#018x}, is {there_now:#018x}{} | \
             [{standing:#x}] is {:#018x}",
            shadow.words[i],
            if there_now == shadow.words[i] { "" } else { "  <== THE CHECKED FRAME WAS WRITTEN" },
            now[i],
        );
    }
    report_contexts(rsp, Some(shadow.ctx as u64));
    panic!(
        "context_switch: the seven words it is about to pop are not the seven words \
         `check_switch_frame` validated, or the stack pointer it is standing on is not \
         the one that was checked"
    );
}

/// Corrupts the incoming frame once, at the [`MUTATE_AT`]th switch, to prove [`switch_witness_verify`] fires.
/// # Safety
/// Only built under a mutation feature; never a kernel booted for any other purpose.
#[cfg(any(feature = "switch-witness-mutate-frame", feature = "switch-witness-mutate-rsp"))]
unsafe fn switch_witness_mutate(restore: *const KernelCtx) {
    /// Switch count before the one mutation; small enough that every boot reaches it.
    const MUTATE_AT: u64 = 8;
    static SWITCHES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    if SWITCHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed) != MUTATE_AT {
        return;
    }
    // SAFETY: `restore` is a live `KernelCtx` from this kernel's own pass.
    let rsp = unsafe { (*restore).rsp };
    #[cfg(feature = "switch-witness-mutate-frame")]
    // SAFETY: the `rbx` slot of the frame `check_switch_frame` has just validated.
    unsafe {
        core::ptr::write_volatile((rsp + 32) as *mut u64, 0xdead_beef_dead_beef)
    };
    #[cfg(feature = "switch-witness-mutate-rsp")]
    // `+8`, not a larger offset, keeps the write inside the valid stack, so the arm is settled by
    // the report and not by a different crash.
    // SAFETY: as above, and the field is this context's own.
    unsafe {
        core::ptr::write_volatile(&raw const (*restore).rsp as *mut u64, rsp + 8)
    };
}

impl Hw for KernelHw {
    type Payload = KernelPayload;

    /// Order is forced: outgoing per-CPU state must be captured, and incoming CR3/TSS/stack installed, before `rsp` moves — after that this frame no longer exists.
    ///
    /// Until [`context_switch`] writes the outgoing `rsp`, it is `answer_steal_requests`, not this
    /// function, that keeps another CPU out of that window.
    unsafe fn switch(&self, token: RunToken<KernelPayload>) {
        let save = token.save_ptr();
        let restore = token.restore_ptr();
        // SAFETY: `save`/`restore` are live Box-backed contexts from `SchedPass::finish`, freed only by a later pass; `incoming.fs_base` is this kernel's own canonical value for the thread being installed.
        unsafe {
            (*save).fs_base = cpu::read_fs_base();
            (*save).preempt = crate::preempt::count();
            let incoming: &KernelCtx = &*restore;
            // The only load of `incoming.rsp`: reading it again after `cr3.activate()`'s clobber would be a second, unguarded load.
            let rsp = check_switch_frame(incoming, &token);
            #[cfg(any(
                feature = "switch-witness-mutate-frame",
                feature = "switch-witness-mutate-rsp"
            ))]
            switch_witness_mutate(restore);
            crate::preempt::set_count(incoming.preempt);
            percpu::set_current_tid(incoming.id.map(|id| id.1));
            percpu::set_current_pid(incoming.id.map(|id| id.0));
            match incoming.id {
                Some(_) => {
                    // Here, not in the pass: this is the one place a task (not idle) becomes what a
                    // CPU runs, which `note_dispatch` below must count for `heartbeat`'s `ran=` to
                    // be meaningful.
                    #[cfg(feature = "boot-actuators")]
                    crate::heartbeat::note_dispatch();
                    percpu::set_kernel_stack(incoming.kernel_stack_top);
                    incoming.cr3.activate();
                    cpu::write_fs_base(incoming.fs_base);
                }
                // idle's stack top is per-CPU, unknowable at boot-time init, so it is read here instead.
                None => {
                    percpu::set_kernel_stack(percpu::idle_stack_top());
                    incoming.cr3.activate();
                }
            }
            RUNNING_CTX[percpu::cpu_id() as usize]
                .store(restore as u64, core::sync::atomic::Ordering::Relaxed);
            context_switch(&raw mut (*save).rsp, rsp);
        }
    }

    /// Reached once per task, from a later pass running on another stack, so dropping `payload`
    /// here never frees the stack this call stands on; `publish_released` must be last — a
    /// retirer's wait ends only once this drop has happened.
    fn release(&self, _key: TaskKey, payload: KernelPayload, acct: TaskAccounting) {
        let handle = payload.handle.clone();
        handle.finalize(acct);
        drop(payload);
        handle.publish_released();
    }
}
