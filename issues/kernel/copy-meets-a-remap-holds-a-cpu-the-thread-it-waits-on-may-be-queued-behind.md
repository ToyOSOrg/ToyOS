---
status: expected-red
kind: defect
opened: 2026-09-28
---

# `copy-meets-a-remap` holds a CPU with `IF` clear, waiting for a thread that may be queued behind it on that CPU

`kernel/src/user_ptr.rs`'s `remap_race::hold` spins inside the copier's
syscall with interrupts off until the racing process maps again, and panics
after `BOUND` (10 s). The thread that has to map is the program's main thread.
Nothing puts it on another CPU: `CpuHandles::place` puts the copier on the
least-loaded CPU that is answering, and a steal is a `StealRequest` that only
the victim's own pass answers (`SchedPass::answer_steal_requests`). A CPU
spinning with `IF` clear takes no pass. If the main thread is queued on that
CPU, no other CPU can take it, and the kernel panics.

Seen once, in the orchestrator's Fast tier for PR #562 at `2a9c77ee` (a
two-CPU guest):

```
[kernel 10.738 cpu1] sched: cpu=1 ready=0 dying=0 stopped=0 parked=4 current=None trips=91
[kernel 11.142 cpu0 tid=1] PANIC: panicked at src/user_ptr.rs:402:13:
copy-meets-a-remap: pid 6 held a copy 10000ms and never mapped again
  cpu0 is on ctx … pid=6 tid=1     (the copier, in `copy_out::<SchedInfo>` → `remap_race::hold`)
  cpu1 is on ctx … pid=3 tid=0
```

cpu1 was idle with nothing ready, and the main thread (pid 6 tid 0) was on
neither CPU. Queued behind the spin on cpu0 is the reading that fits all of
this. Nothing in the capture proves it: no line says which queue held the
thread.

This is not PR #562's doing. On that branch `kernel/src/user_ptr.rs` and the
scheduler are the same as on `main`. The one change to
`copy_out_races_munmap.rs` removes a 10 s assert from the main thread's cue
loop, and that assert only fires when the main thread is running, in which
case it sees the cue and maps.

## Exit condition

The hold cannot strand the thread it waits for. For example: the racing thread
is placed on a different CPU from the copier before the cue, or the hold waits
with the CPU able to run passes. `user_copy_races_munmap` is then green, and a
mutation that puts both threads on one CPU reds with a line saying so rather
than with this panic. Then this file and its `src/redlist.rs` row are deleted.

## Owner

`kernel/src/user_ptr.rs` `remap_race`, `tests/toyos-rust-tests/src/bin/copy_out_races_munmap.rs`. Nobody holds it.
