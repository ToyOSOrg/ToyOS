---
status: open
kind: defect
opened: 2026-09-14
---

# The machine stop says how many threads it waited for and never which

`kernel/src/quiesce.rs`'s `stop` sweeps the process table, parks until a thread
it names stops, parks or exits, and sweeps again until every one has. The record it writes
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

## What the dev host measures, and what the budget could be

Twelve boots of `quiesce_stops_the_machine` at `feffffb6` — a kernel that swept
on a 10 ms cadence rather than on its threads' transitions — `tests/quiescecase`,
six writer threads on two vcpus:

| host | park, per boot |
|---|---|
| quiet | 10, 10, 10, 20, 169 ms |
| loaded, 24 spinners, load 36-95 | 10, 10, 10, 12, 26, 156, 158 ms |

All twelve read `9 of 9` stopped and `0` open, so none is a reading of a stop
that struggled. `quiesce::PARK` is 2,010 ms, and the worst of them spends 8% of
it — on the quiet host, which is what says the quantity is not a function of
host load alone.

The budget cannot simply be widened to the bound that would cover a thread
inside a run of block work: `block::DEADMAN` is 120 s and the boot deadline is
armed straight through `quiesce` — `kernel/src/deadline.rs` stands down only for
a panic — so a park that long is sealed as a wedge rather than expiring with the
record this issue is about.

## Why it is not free to answer

The record is a fixed line rendered by `toyos-quiesce` and read back by
`src/metal.rs` and the harness, and the profile prices two numbers out of it.
Naming a thread means the record carries a pid, a tid and the class of wait
that thread was in — which the sweep does not collect today, because it counts
rather than remembers.

## What would show it

The record naming the last thread to stop, and a boot of `tests/metalcase`
whose stop time can then be attributed to it rather than inferred. Until the
record names one, its `in N ms` explains nothing about itself, and no profile
row prices it.
