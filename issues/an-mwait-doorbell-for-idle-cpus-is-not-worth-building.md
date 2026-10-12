---
status: none
kind: rejected
opened: 2026-10-12
---

# An MWAIT doorbell for idle CPUs is not worth building

An idle CPU waits in `sti; hlt` (`kernel/src/arch/x86_64/hw.rs:21`), so a
wake posted to it rings its doorbell (`kernel/pure/sched/mailbox.rs:474-490`)
and then sends a kick IPI (`kernel/src/arch/x86_64/apic.rs:256-260`). Waiting
in MWAIT on the doorbell's line instead would let the store alone wake it.
That saves at most the IPI's send and delivery, and those are under a fifth
of an idle wake on both proving machines, so the doorbell is dropped by the
kill condition fixed before it was measured: not worth building if send plus
delivery is under 20 % of the wake.

**Measured** (`wt/toyos-scout-tlb` at `d4c2f8ebe`, `main`'s `177e72a9e` plus
the scout; https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198).
A user ping-pong between CPUs 4 and 5, each wake split from the poster's
send to the woken task running, p50 in TSC ticks:

| | T14 (x2APIC, 2419 MHz) | AMD bench (xAPIC, 1796 MHz) |
|---|---|---|
| send | 189 | under one 18-tick quantum |
| delivery, skew-free | 966 | 1062 |
| whole wake | 12029 and 11854 | 5796 and 5724 |
| (send + delivery) / wake | 9.6 % and 9.7 % | 18.3 % and 18.5 % |

Delivery includes `hlt`'s C1 exit, which an MWAIT wait pays too, so the share
is an upper bound on what the doorbell could save. What an idle CPU's wait
costs beyond its wake, the state it sleeps in, is
`issues/idle-cpus-enter-the-c-states-cst-names.md`'s.
