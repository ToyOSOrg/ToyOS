---
status: open
kind: defect
opened: 2026-09-14
---

# The machine stop says how many threads it waited for and never which

`kernel/src/quiesce.rs`'s `stop` sweeps the process table, waits a cadence, and
sweeps again until every userland thread it must stop has. The record it writes
says how many threads there were and how long they took; it does not say
**which** one the time went on.

## What the machine measured

Run 41, `tests/metalcase` on the T14, at `52c8a3f9` — before the sweep had a
cadence, so the sweep count is that kernel's and not this one's:

```
[20:38:12 7.955 cpu4] stop: 7 of 7 userland thread(s) stopped across 8 cpu(s)
in 129 ms over 67953 sweep(s), 0 block operation(s) still open
```

The same kernel on the same machine, `tests/metaldevicecase`, one boot later:

```
[20:39:43 13.896 cpu6] stop: 5 of 5 userland thread(s) stopped across 8 cpu(s)
in 0 ms over 1 sweep(s), 0 block operation(s) still open
```

129 ms against 0 ms, and nothing in either file says what the difference was.
What can be established from the log is only this:

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

## Why it is not free to answer

The record is a fixed line rendered by `toyos-quiesce` and read back by
`src/metal.rs` and the harness, and the profile prices two numbers out of it.
Naming a thread means the record carries a pid, a tid and the class of wait
that thread was in — which the sweep does not collect today, because it counts
rather than remembers.

## What would show it

The record naming the last thread to stop, and a boot of `tests/metalcase`
whose `park_ms` can then be attributed to it rather than inferred. Until the
record names one, `boot.metalcase.park_ms` is priced at `quiesce::PARK`'s
derivation and the reading beside it explains nothing about itself.
