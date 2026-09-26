//! Per-CPU state. On AArch64 it is reached through `TPIDR_EL1`, and the block
//! it names is built by the port's stage 5 (other CPUs) on top of stage 4's
//! exception entry; until then every accessor is owed. The boot never reaches
//! one: the log and the panic path read [`crate::log::PERCPU_READY`] first.

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

pub fn cpu_id() -> u32 {
    owed!("per-CPU state", "stage 4")
}

pub fn current_tid() -> Option<Tid> {
    owed!("per-CPU state", "stage 4")
}

pub fn set_current_tid(_tid: Option<Tid>) {
    owed!("per-CPU state", "stage 4")
}

pub fn current_pid() -> Option<Pid> {
    owed!("per-CPU state", "stage 4")
}

pub fn set_current_pid(_pid: Option<Pid>) {
    owed!("per-CPU state", "stage 4")
}

/// # Safety
/// `top` is the top of the kernel stack the next entry from user mode lands on.
pub unsafe fn set_kernel_stack(_top: u64) {
    owed!("per-CPU state", "stage 4")
}

/// # Safety
/// Read on the CPU whose entry stacks they are.
pub unsafe fn entry_stacks() -> (u64, u64) {
    owed!("per-CPU state", "stage 4")
}

pub fn idle_stack_top() -> u64 {
    owed!("per-CPU state", "stage 4")
}

#[cfg(feature = "test-actuators")]
pub fn idle_guard_byte() -> u64 {
    owed!("per-CPU state", "stage 4")
}

#[cfg(feature = "test-actuators")]
pub fn idle_stack_size() -> usize {
    owed!("per-CPU state", "stage 4")
}

#[cfg(feature = "test-actuators")]
pub fn idle_stack_high_water() -> usize {
    owed!("per-CPU state", "stage 4")
}

pub fn in_syscall() -> bool {
    owed!("per-CPU state", "stage 4")
}

pub fn syscall_num() -> u64 {
    owed!("per-CPU state", "stage 4")
}

pub fn swap_fault_state(_new: CpuFaultState) -> CpuFaultState {
    owed!("per-CPU state", "stage 4")
}

/// This CPU's shard, its identity, and one sequence number out of that shard.
pub fn reserve_log_slot(
    _guard: &crate::arch::IrqGuard,
) -> (*const crate::log::Shard, u64, u32, u32, u32) {
    owed!("per-CPU state", "stage 4")
}

pub fn irq_counts_here(_first: usize, _second: usize) -> (u64, u64) {
    owed!("per-CPU state", "stage 4")
}

pub fn preempt_count() -> u32 {
    owed!("per-CPU state", "stage 4")
}

pub fn set_preempt_count(_value: u32) {
    owed!("per-CPU state", "stage 4")
}

pub fn preempt_count_up() {
    owed!("per-CPU state", "stage 4")
}

pub fn preempt_count_down() {
    owed!("per-CPU state", "stage 4")
}

pub fn resched_owed() -> bool {
    owed!("per-CPU state", "stage 4")
}

pub fn set_resched_owed(_owed: bool) {
    owed!("per-CPU state", "stage 4")
}

pub fn faulting() -> bool {
    owed!("per-CPU state", "stage 4")
}
