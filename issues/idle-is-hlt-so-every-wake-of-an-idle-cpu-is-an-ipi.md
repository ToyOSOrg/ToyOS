---
status: open
kind: track
opened: 2026-10-12
---

# Idle is `hlt`, so every wake of an idle CPU is an IPI

An idle CPU waits in `sti; hlt` (`kernel/src/arch/x86_64/hw.rs:21`), so the
only thing that ends its wait is an interrupt: a wake posted to it rings its
doorbell (`kernel/pure/sched/mailbox.rs:474-490`) and then sends a kick IPI
(`kernel/src/arch/x86_64/apic.rs:256-260`). A CPU that waits in MWAIT on its
doorbell's line (CPUID.01H:ECX[3]) is woken by the store alone. Nothing
measures what the IPI and the exit cost. Round 1 of PR #861 on the T14, in
its `main` column (main's kernel plus the instrument; that PR's body, "Round 1
on the T14: main, D and Ubuntu"), counts 178.1 idle exits, 118.0 kicks and
236.0 switches per MB over 60.1 netstack passes, about 3, 2 and 4 a pass, and
1.40 Mcyc/MB of kernel outside the app and netstack, about 23 kcyc a pass
(derived), without splitting them.
`issues/idle-cpus-enter-the-c-states-cst-names.md` is which state the wait
enters; this file is what ends it. The doorbell shares a line with its
neighbours' published counters
(`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`),
so a monitor on it today would also wake on their writes.

It waits on the T14's idle-exit split: the IPI's send, its delivery and the
pass it starts, each timed.

**Exit**: that split, posted; an idle CPU on each proving machine that
enumerates MONITOR/MWAIT woken by its doorbell's store with no IPI; and the
T14's pipe round trip of
`issues/no-program-measures-toyos-against-linux-on-one-machine.md` read with
it and without. Where the round trip is no faster with it, this file becomes
`rejected` with both readings instead.
