---
status: open
kind: defect
opened: 2026-08-27
---

# The blocked-task dump asserts it is not under a lock, and `pass_block` calls it under one

Seen once on 2026-08-27, dev host, full fast tier twelve wide with a second
worktree's suite on the machine, on `wt/toyos-md1` at `03af5421` — a branch
carrying no kernel byte. `blocked_dump` red on a machine-wide death:

```
FAIL blocked_dump: kernel panic: PANIC: panicked at src/sched/dump.rs:198:5:
  — the guest went quiet because every CPU is halted ... It was waiting for the whole report
[kernel 2.262 cpu5] PANIC: panicked at src/sched/dump.rs:198:5:
the blocked-task dump ran under a lock: preempt depth 2
```

**The backtrace is the finding and it names the path**, so this is not the
"a machine-wide death reds whichever test was running" class — the panic is in
the code `blocked_dump` exists to drive:

```
kernel::sched::dump::request+0x69
kernel::sched::driver::drain_irqs+0x2f
kernel::sched::driver::pass_block+0x3c
kernel::completion::wait_inner+0x236
kernel::completion::wait_until::<kernel::inbox::submit::{closure#0}>+0xcd
kernel::inbox::submit+0xda2
kernel::arch::syscall::ipc::sys_inbox_submit+0x8e
```

`Ctrl+Alt+D` arrives as an interrupt, `drain_irqs` services the dump request
from inside `pass_block`, and `pass_block` is reached from
`completion::wait_inner` with a preempt depth of 2. `dump::request`'s own
assertion says the dump may not run under a lock, so the two are in direct
contradiction whenever the key lands on a CPU blocked in `inbox::submit`.
`cpu5` was the compositor (`Process: compositor pid=5 state=Live`,
`Syscall: num=90`).

The harness's second half says the same thing from outside:
`Ctrl+Alt+D produced no complete report`.

**Load is the trigger and not the cause.** Alone the same test is green in 3 s.
What load changes is the chance that the injected key arrives while some CPU is
inside `inbox::submit` — which is a window a quiet machine rarely holds and a
loaded one holds often. The owner's ruling that host load is not an excuse is
what this entry is written under: the assertion fires on a state the kernel can
reach, and re-running is not an answer to it.

`cargo run -- --known-red blocked_dump` answers `KNOWN-RED`, and **neither row
is this**: one is the census half and `/system/bin/terminal` racing the compositor
(CI, 2 of 5, 2026-08-08), the other is `nothing typed at the terminal window
reached a shell` on a loaded dev host. Both are wall-clock guards reporting the
content they were going to assert. This is an assertion inside the kernel,
with a backtrace.

What is owed: either `dump::request` may run at this depth and its assertion is
wrong, or `drain_irqs` may not service a dump request from `pass_block` and the
request has to be deferred to a pass that holds nothing. Deciding which is the
scheduler's owner's, and the answer is one of the two rather than a wider
assertion.
