//! Per-CPU state, reached through `TPIDR_EL1`, which holds this CPU's
//! [`PerCpu`] from [`install`] on and which nothing else writes. EL0 cannot
//! read it, so a thread learns nothing of the kernel's layout from it.
//!
//! Every field another context on the same CPU can write — an interrupt, a
//! nested exception — is an atomic, so the whole block is only ever reached
//! through a shared reference, and an increment is one exclusive-monitor loop
//! an exception on this CPU cannot split (Arm ARM K.a, B2.17.5: taking an
//! exception clears the monitor).

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};

use alloc::boxed::Box;

use crate::log;
use crate::process::{Pid, Tid};

/// Per-CPU fault state machine for the escalation policy on nested faults.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CpuFaultState {
    Normal = 0,
    PageFault = 1,
    Fatal = 2,
    Panic = 3,
}

/// One CPU's block. Only this CPU writes it, bar [`irq_counts`](Self::irq_counts),
/// which `irq_census` reads from any.
pub struct PerCpu {
    cpu_id: u32,
    /// `u32::MAX` when no thread runs here.
    current_tid: AtomicU32,
    current_pid: AtomicU32,
    preempt_count: AtomicU32,
    need_resched: AtomicU8,
    fault_state: AtomicU8,
    /// Timer interrupts taken at EL1, which only count and ask for a pass.
    kernel_timer_fires: AtomicU32,
    last_seen_kernel_timer_fires: AtomicU32,
    /// Counter ticks the timer was last armed for: what a timer interrupt
    /// taken at EL0 re-arms it with, and zero when it is stopped.
    armed_ticks: AtomicU64,
    /// The kernel stack a switch last installed, for the stack witness: on
    /// AArch64 an entry from EL0 lands on `SP_EL1` as the last `ERET` left it,
    /// and nothing here points it anywhere.
    kernel_stack: AtomicU64,
    idle_stack_top: u64,
    /// What the last entry by `SVC` said, for a crash report: its return
    /// address, number, frame pointer and stack pointer.
    syscall_pc: AtomicU64,
    syscall_num: AtomicU64,
    syscall_fp: AtomicU64,
    syscall_sp: AtomicU64,
    /// The task whose syscall this CPU is inside: packed pid:tid, or [`NO_SYSCALL`].
    syscall_task: AtomicU64,
    /// This CPU's [`log::Shard`]: the boot shard on CPU 0.
    log_shard: &'static log::Shard,
    /// One counter per `irq_census::Source`, and the total.
    irq_counts: [AtomicU64; crate::irq_census::SLOTS],
}

/// This CPU's block.
#[inline]
fn this() -> &'static PerCpu {
    let block: u64;
    // SAFETY: reads `TPIDR_EL1`, which [`install`] points at a leaked `PerCpu`
    // before any accessor here runs on this CPU: `log::PERCPU_READY` gates them
    // on the boot CPU, and an AP installs its block before it does anything else.
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el1", out(reg) block, options(nomem, nostack, preserves_flags));
        &*(block as *const PerCpu)
    }
}

/// Build CPU `cpu_id`'s block, on the boot CPU and before that CPU runs: its
/// log shard and interrupt counters are published here, so no reader misses them.
pub fn alloc(cpu_id: u32) -> &'static PerCpu {
    let block: &'static PerCpu = Box::leak(Box::new(PerCpu {
        cpu_id,
        current_tid: AtomicU32::new(u32::MAX),
        current_pid: AtomicU32::new(u32::MAX),
        preempt_count: AtomicU32::new(0),
        need_resched: AtomicU8::new(0),
        fault_state: AtomicU8::new(CpuFaultState::Normal as u8),
        kernel_timer_fires: AtomicU32::new(0),
        last_seen_kernel_timer_fires: AtomicU32::new(0),
        armed_ticks: AtomicU64::new(0),
        kernel_stack: AtomicU64::new(0),
        idle_stack_top: crate::sched::idle_stack::alloc(),
        syscall_pc: AtomicU64::new(0),
        syscall_num: AtomicU64::new(0),
        syscall_fp: AtomicU64::new(0),
        syscall_sp: AtomicU64::new(0),
        syscall_task: AtomicU64::new(NO_SYSCALL),
        log_shard: log::shard_for(cpu_id),
        irq_counts: [const { AtomicU64::new(0) }; crate::irq_census::SLOTS],
    }));
    crate::irq_census::publish(cpu_id, block.irq_counts.as_ptr());
    block
}

/// Make `block` this CPU's through `TPIDR_EL1`; after it, every accessor here
/// answers on this CPU. The thread registers EL0 can read are cleared, so a
/// thread's first read of one finds nothing firmware left.
pub fn install(block: &'static PerCpu) {
    // SAFETY: the block lives for the machine's life; `TPIDRRO_EL0` and
    // `TPIDR_EL0` are EL0's to read and hold nothing of the kernel's.
    unsafe {
        core::arch::asm!(
            "msr tpidr_el1, {}",
            "msr tpidrro_el0, xzr",
            "msr tpidr_el0, xzr",
            in(reg) block as *const PerCpu as u64,
            options(nostack, preserves_flags),
        );
    }
}

/// The boot CPU's block, installed.
pub fn init_bsp() {
    install(alloc(0));
    crate::log::PERCPU_READY.store(true, core::sync::atomic::Ordering::Release);
    log!("percpu: BSP cpu_id=0 mpidr={:#x}", super::cpu::hardware_id());
}

pub fn cpu_id() -> u32 {
    this().cpu_id
}

/// `None` means idle.
pub fn current_tid() -> Option<Tid> {
    match this().current_tid.load(Relaxed) {
        u32::MAX => None,
        raw => Some(Tid::from_raw(raw)),
    }
}

pub fn set_current_tid(tid: Option<Tid>) {
    this().current_tid.store(tid.map_or(u32::MAX, |t| t.raw()), Relaxed);
}

/// `None` means idle.
pub fn current_pid() -> Option<Pid> {
    match this().current_pid.load(Relaxed) {
        u32::MAX => None,
        raw => Some(Pid::from_raw(raw)),
    }
}

pub fn set_current_pid(pid: Option<Pid>) {
    this().current_pid.store(pid.map_or(u32::MAX, |p| p.raw()), Relaxed);
}

/// Record the stack the next entry from EL0 lands on: the one a switch just
/// installed, which `SP_EL1` already is.
/// # Safety
/// Called on the CPU whose block it is.
pub unsafe fn set_kernel_stack(top: u64) {
    this().kernel_stack.store(top, Relaxed);
}

/// The stack an entry from EL0 lands on, twice: AArch64 has one where x86-64
/// has `kernel_rsp` and `tss.rsp0`.
/// # Safety
/// Read on the CPU whose entry stacks they are.
#[cfg(feature = "stack-witness")]
pub unsafe fn entry_stacks() -> (u64, u64) {
    let top = this().kernel_stack.load(Relaxed);
    (top, top)
}

pub fn idle_stack_top() -> u64 {
    this().idle_stack_top
}

/// The last byte of this CPU's idle guard page — the first byte an overflow reaches.
#[cfg(feature = "test-actuators")]
pub fn idle_guard_byte() -> u64 {
    idle_stack_top() - crate::sched::idle_stack::SIZE as u64 - 1
}

/// No task on this CPU is inside a syscall; [`pack_task`] never produces this value.
const NO_SYSCALL: u64 = u64::MAX;

fn pack_task(pid: u32, tid: u32) -> u64 {
    (u64::from(pid) << 32) | u64::from(tid)
}

/// Enter this CPU's syscall bracket, with what the entry saw. `super::trap` is the only caller.
pub(super) fn enter_syscall(pc: u64, num: u64, fp: u64, sp: u64) {
    let block = this();
    block.syscall_pc.store(pc, Relaxed);
    block.syscall_num.store(num, Relaxed);
    block.syscall_fp.store(fp, Relaxed);
    block.syscall_sp.store(sp, Relaxed);
    block.syscall_task.store(pack_task(block.current_pid.load(Relaxed), block.current_tid.load(Relaxed)), Relaxed);
}

/// …and leave it.
pub(super) fn leave_syscall() {
    this().syscall_task.store(NO_SYSCALL, Relaxed);
}

/// Whether the task this CPU runs is inside a syscall now.
pub(super) fn in_syscall() -> bool {
    let block = this();
    let recorded = block.syscall_task.load(Relaxed);
    recorded != NO_SYSCALL
        && recorded == pack_task(block.current_pid.load(Relaxed), block.current_tid.load(Relaxed))
}

/// The last syscall's return address, frame pointer and stack pointer;
/// meaningful only while [`in_syscall`] holds.
pub(super) fn syscall_context() -> (u64, u64, u64) {
    let block = this();
    (block.syscall_pc.load(Relaxed), block.syscall_fp.load(Relaxed), block.syscall_sp.load(Relaxed))
}

pub fn syscall_num() -> u64 {
    this().syscall_num.load(Relaxed)
}

/// Not atomic as a pair, and needs not be: only exception and panic entry
/// touch it, with interrupts masked.
pub fn swap_fault_state(new: CpuFaultState) -> CpuFaultState {
    match this().fault_state.swap(new as u8, Relaxed) {
        0 => CpuFaultState::Normal,
        1 => CpuFaultState::PageFault,
        2 => CpuFaultState::Fatal,
        _ => CpuFaultState::Panic,
    }
}

pub fn set_fault_state(new: CpuFaultState) {
    this().fault_state.store(new as u8, Relaxed);
}

/// This CPU's shard, its identity, and one sequence number out of that shard.
pub fn reserve_log_slot(guard: &crate::arch::IrqGuard) -> (*const log::Shard, u64, u32, u32, u32) {
    let block = this();
    let (shard, cpu, tid, pid) =
        (block.log_shard, block.cpu_id, block.current_tid.load(Relaxed), block.current_pid.load(Relaxed));
    // SAFETY: `guard` masks this CPU, the only one that reserves in its shard.
    let seq = unsafe { shard.reserve(guard) };
    (shard, seq, cpu, tid, pid)
}

/// One delivery of `source`, counted in this CPU's block.
pub(super) fn irq_took(source: crate::irq_census::Source) {
    let counts = &this().irq_counts;
    counts[crate::irq_census::TOTAL].fetch_add(1, Relaxed);
    counts[1 + source as usize].fetch_add(1, Relaxed);
}

/// Two of this CPU's interrupt counters.
pub fn irq_counts_here(first: usize, second: usize) -> (u64, u64) {
    let counts = &this().irq_counts;
    (counts[first].load(Relaxed), counts[second].load(Relaxed))
}

#[inline]
pub fn preempt_count() -> u32 {
    this().preempt_count.load(Relaxed)
}

#[inline]
pub fn set_preempt_count(value: u32) {
    #[cfg(feature = "mask-windows")]
    let old = preempt_count();
    this().preempt_count.store(value, Relaxed);
    #[cfg(feature = "mask-windows")]
    crate::windows::preempt_set(old, value);
}

/// One increment, atomic against an interrupt on this CPU.
#[inline]
pub fn preempt_count_up() {
    this().preempt_count.fetch_add(1, Relaxed);
    #[cfg(feature = "mask-windows")]
    crate::windows::preempt_raised();
}

#[inline]
pub fn preempt_count_down() {
    #[cfg(feature = "mask-windows")]
    crate::windows::preempt_lowering();
    this().preempt_count.fetch_sub(1, Relaxed);
}

#[inline]
pub fn resched_owed() -> bool {
    this().need_resched.load(Relaxed) != 0
}

#[inline]
pub fn set_resched_owed(owed: bool) {
    this().need_resched.store(u8::from(owed), Relaxed);
}

/// Whether this CPU is inside a fault or panic.
#[inline]
pub fn faulting() -> bool {
    this().fault_state.load(Relaxed) != 0
}

/// Timer interrupts taken at EL1.
pub fn kernel_timer_fires() -> u32 {
    this().kernel_timer_fires.load(Relaxed)
}

pub fn last_seen_kernel_timer_fires() -> u32 {
    this().last_seen_kernel_timer_fires.load(Relaxed)
}

pub fn set_last_seen_kernel_timer_fires(v: u32) {
    this().last_seen_kernel_timer_fires.store(v, Relaxed);
}

pub(super) fn note_kernel_timer_fire() {
    this().kernel_timer_fires.fetch_add(1, Relaxed);
}

/// What the timer was last armed for, in counter ticks; zero is stopped.
pub(super) fn armed_ticks() -> u64 {
    this().armed_ticks.load(Relaxed)
}

pub(super) fn set_armed_ticks(ticks: u64) {
    this().armed_ticks.store(ticks, Relaxed);
}
