---
status: open
kind: defect
opened: 2026-08-08
---

# A userland process reaches its first line half a second after its sibling on a runner, and ~30 ms after it here

Fell out of the null-sink probe and is not what it closed. On all three reps the two
programs `tests/testcases` starts — soundd and test-runner, spawned 1–3 ms apart
— printed their first lines **0.53–0.56 s apart**, and which of the two was
first flipped between reps. On this host the same pair is ~30 ms apart, in spawn
order, every time. The kernel's own boot is the same speed on both (`Boot:
complete` at 275–304 ms on the runner against 269 ms here), so it is not a slow
machine: it is the first moment two runnable tasks exist.

The i8042 verdict measures the same thing from the kernel's side, since it is
emitted from the first idle-loop trip after arming: `idle at` 523–552 ms on the
runner against 304 ms here.

Nothing here says whether that is the host descheduling a vCPU thread, something
in userland startup, or the scheduler leaving a task unclaimed — the probe was
not built to tell them apart. It is recorded because a half-second of skew
between two init children is enough to decide any remaining wall-clock margin in
the suite, and because it is invisible on a host whose TCG runs one vCPU at a
time.

**One of the three candidates has narrowed, and nothing here has been
re-measured** (read 2026-08-24). `Balance::PushOnSurplus` ships as of the owner's
2026-08-23 decision (`toyos-sched/src/cpu.rs`, `Balance`): the pull half was
one-shot, so a CPU reaching its idle pass while every sibling still published
zero surplus halted with no probe outstanding and nothing in the protocol woke
it — which is exactly the shape "the scheduler leaving a task unclaimed"
describes. Measured on a lopsided machine at 20 seeds per width, 0 of 20 seeds
reached every CPU at eight under pull and 20 of 20 under the push.

That is a reason to re-measure and not a verdict on this entry: the skew above
was seen on a CI runner and is ~30 ms on the dev host, so nothing a TCG host
running one vCPU at a time can produce decides it. **The deciding measurement is
this probe re-run on a runner** — two init children's first lines, and the
i8042's `idle at` from the same boot as the kernel-side reading of the same
quantity.
