---
status: open
kind: defect
opened: 2026-07-31
---

# A syscall runs with preemption disabled from entry to exit, only incidentally

`syscall_entry` raises the preempt count before `call {handler}` and lowers it
after, so `preempt::enable`'s `count() == 0` slow path can never fire inside a
syscall — no matter how many locks the handler takes and drops. The scheduler's
stated model counts that slow path as an RT-wake safe point and bounds wake
latency by "the longest preempt-disabled section"; in syscall context that
section is *the entire syscall*, and the real bound is the next
`kernel_exit_to_user_check`.

A syscall's body runs with interrupts open on both architectures: the gate
masks them only from the entry through the user-state save and from the
handler's return to the user return (`kernel/src/arch/x86_64/syscall.rs`,
`kernel/src/arch/aarch64/trap.rs`). A timer's expiry or a kick inside one sets
`need_resched`, which the syscall's exit serves, and the Ring 0 expiry re-arms
one quantum on. What still runs masked is the Ring 3 tick's and kick's pass,
which the handler runs, and the walk under an `IrqWatch`'s lock
(`issues/kernel/a-process-lengthens-an-interrupts-off-walk-by-the-threads-it-parks-on-one-ring.md`).

This was masked until the preempt count was made conserved across a context
switch (the scheduler's own baselines needed it): before that the count drifted,
so a lock drop inside a syscall reached zero at random and preempted at random.
The behaviour is now deterministic, and deterministically weaker than the model
assumes.

Owner: `issues/kernel/toyos-beats-linuxs-latency-on-the-t14.md`, whose second
step is syscalls running with interrupts on (owner, 2026-10-03).

**Exit**, on the T14: the longest interrupts-off window the `mask_windows` row
reads under its load (`herd irqs_off_ns=`, printed by `windows_on_metal` in
`tests/toyos.rs`) is no longer than 131 µs, the track's ruled bar: a masked
window makes a timer's interrupt late by at most its own length. The row
cannot read it before step 1: a `mask-windows` kernel charges each of the
firmware's 4.5 ms SMIs
(`issues/hardware/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`)
to whatever window it lands in.
