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
measures what the IPI and the exit cost: round 1 of PR #861 counts 3 idle
exits, 2 kicks and 4 switches per netstack pass on the T14, about 22.6 kcyc of
kernel per pass, without splitting them (its body, "Round 1 on the T14: main,
D and Ubuntu"), and round 3 reads its idle window burning 36 to 59 Mcyc/s
against Ubuntu's 13.1 ("Round 3 on the T14").
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
