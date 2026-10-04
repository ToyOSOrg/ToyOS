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

The T14 read the step at #716 (comment 5979107466; readbacks
`irqon/metal/{base,head}/mask_windows/runN` and `irqon/metal/head-full`),
five `mask_windows` boots an arm, interleaved, and the head's full profile
once, `windows_on_metal`'s `herd` line:

| arm | `irqs_off_ns` | `preempt_off_ns` |
|---|---|---|
| base, main at `d47b383cf` | 18863127, 1185364, 1084719, 1265962, 4703201 | 18862957, 1185277, 1065082, 1265899, 4703058 |
| head, images at `f89e73128` | 60479, 62947, 56462, 57842, 58396; 59488 | 1175425, 4711060, 4702163, 4714590, 9806929; 10784336 |

The interrupts-off window is under 131 µs on every head boot; the
preemption-off window is not on any. The three head readings of 4.70 to
4.71 ms are an SMI's window by reading, no SMI count being read in that boot;
nothing names the 9.8 and 10.8 ms ones.

**Exit**, on the T14: the longest preemption-off window the `mask_windows` row
reads under its load (`herd preempt_off_ns=`, printed by `windows_on_metal` in
`tests/toyos.rs`) is no longer than 131 µs, the track's ruled bar. The row
cannot read it before step 1: a `mask-windows` kernel charges each of the
firmware's 4.5 ms SMIs
(`issues/hardware/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`)
to whatever window it lands in. This exit is the orchestrator's amendment at
#716's first review: it read the interrupts-off window, which step 2 shortened
while the defect this file names, preemption off for a whole syscall, stood.
