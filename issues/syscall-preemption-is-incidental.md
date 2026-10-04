---
status: open
kind: defect
opened: 2026-07-31
---

# A syscall runs with interrupts masked, and only incidentally with preemption disabled

`syscall_entry` raises the preempt count before `call {handler}` and lowers it
after, so `preempt::enable`'s `count() == 0` slow path can never fire inside a
syscall — no matter how many locks the handler takes and drops. The scheduler's
stated model counts that slow path as an RT-wake safe point and bounds wake
latency by "the longest preempt-disabled section"; in syscall context that
section is *the entire syscall*, and the real bound is the next
`kernel_exit_to_user_check`.

The preempt count is the weaker of two independent blockers, and it is not the
one that decides the bound. `MSR_FMASK = 0x40200` (`arch/syscall.rs:57`) clears
IF on every SYSCALL entry, and nothing on the straight-line syscall path sets it
again — the only `sti`s in the kernel are `cpu::enable_interrupts`
(`arch/cpu.rs:113`, reached from `trap_dispatch`'s #PF arm and from init),
`kernel_exit_to_user_check`'s own yield window (`arch/idt/mod.rs:232`), and the
idle loop (`sched/driver.rs:494`). With IF=0 the CPU cannot even be *told* to
reschedule: `KernelHw::need_resched` (`hw.rs:96-108`) documents that a remote
CPU's `need_resched` byte is unreachable from here, so a remote request is only
deliverable as a kick IPI — an interrupt the masked target will not take until
it leaves the syscall.

That makes the entry level's fix ineffective on its own: dropping it around
blocking-capable handler regions cannot move remote RT wake latency at all
while IF stays 0. A fix has to unmask interrupts over those regions — which
means auditing what each one is safe to be interrupted in — or the bound is
accepted and that model corrected.

This was masked until the preempt count was made conserved across a context
switch (the scheduler's own baselines needed it): before that the count drifted,
so a lock drop inside a syscall reached zero at random and preempted at random.
The behaviour is now deterministic, and deterministically weaker than the model
assumes.

Owner: `issues/toyos-beats-linuxs-latency-on-the-t14.md`, whose second
step is syscalls running with interrupts on (owner, 2026-10-03).

**Exit**, on the T14: the longest interrupts-off window the `mask_windows` row
reads under its load (`herd irqs_off_ns=`, printed by `windows_on_metal` in
`tests/toyos.rs`) is no longer than the longest lateness of the timer's
interrupt in Linux's reading of this machine, which that track keeps: a masked
window makes a timer's interrupt late by at most its own length. No figure is
set here. Which of Linux's figures is the bar is that track's open question,
and what the row reads today, and which syscalls those windows are, is in
`issues/xhci-waits-are-spins.md`, "On the T14".
