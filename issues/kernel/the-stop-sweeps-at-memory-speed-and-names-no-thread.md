---
status: open
kind: defect
opened: 2026-09-14
---

# The machine stop polls at memory speed and cannot say what it waited for

`kernel/src/quiesce.rs`'s `stop` sweeps the process table, yields, and sweeps
again until every userland thread it must stop has. Two things about that loop
are wrong, and one T14 boot shows both.

## What the machine measured

Run 41, `tests/metalcase` on the T14, at `52c8a3f9`:

```
[20:38:12 7.955 cpu4] stop: 7 of 7 userland thread(s) stopped across 8 cpu(s)
in 129 ms over 67953 sweep(s), 0 block operation(s) still open
```

The same kernel on the same machine, `tests/metaldevicecase`, one boot later:

```
[20:39:43 13.896 cpu6] stop: 5 of 5 userland thread(s) stopped across 8 cpu(s)
in 0 ms over 1 sweep(s), 0 block operation(s) still open
```

## The sweep has no cadence

67,953 sweeps in 129 ms is **1.9 µs per sweep**. Each one takes
`process::PROCESS_TABLE`, walks every process's every thread, drops the lock and
calls `yield_now`; the caller's CPU had nothing else runnable, so the yield
returned immediately and the loop ran at memory speed. The machine-wide process
table lock was therefore taken about sixty-eight thousand times in an eighth of
a second.

**That is a plausible cause of the very delay it was measuring.** A thread
whose own safe point is behind that lock — anything finishing a `sys_exit`,
spawning, or reaping — contends with the sweep on every one of those
iterations. The loop may be slowing the thread it is waiting for. The numbers
permit that mechanism; they do not prove it, because of the second defect.

`block::RETRY_SOONEST` is the pattern this wants: a cadence between attempts,
with the wake doing the real work.

## The record names a count and never a thread

`stop:` says how many threads had not stopped and how long it took. It does not
say **which**, so on the boot above there is no way to know what the 129 ms was
spent on. What can be established from the log is only this:

- It was not a *parked* thread. `stop_if_blocked` is a CAS, not a wait, so a
  parked thread is marked on the first sweep that sees it.
- So it was a thread that was running — either in Ring 3, where the kick's IPI
  takes it to `kernel_exit_to_user_check` at its next return, or inside a
  syscall, where its safe point is however far away that syscall's remaining
  work puts it.
- 129 ms is about ten of cpu1's timer periods on that boot (275 timer
  interrupts over 3.55 s), which is more than a kick-to-boundary costs.

That points at a syscall rather than a spinning userland thread, and it is an
inference and not a reading. `tests/metalcase` starts `sshd` and
`tests/metaldevicecase` does not, which is the difference between the two boots
above; `sshd` wrote no `exit:` record on run 41 at all, so it was alive and
running when the stop found it. Which of the seven took the time is not in the
file.

## What would show it

The record naming the last thread to stop — its pid, its tid and the class of
wait it was in — and a second boot of `tests/metalcase` after the sweep grows a
cadence. If 129 ms becomes a cadence-shaped multiple, the loop was contending
with what it waited for; if it stays 129 ms, it was the thread's own progress
and the number is the honest price of that workload.

Until then `boot.metalcase.park_ms` is priced at `quiesce::PARK`'s derivation
(2,010 ms) with 129 ms recorded beside it, because a single reading on one
workload is not a bound.
