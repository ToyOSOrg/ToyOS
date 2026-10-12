---
status: open
kind: track
opened: 2026-10-12
---

# Idle is `hlt`, so every wake of an idle CPU is an IPI

An idle CPU waits in `sti; hlt` (`kernel/src/arch/x86_64/hw.rs:21`), so the
only thing that ends its wait is an interrupt: a wake posted to it rings its
doorbell (`kernel/pure/sched/mailbox.rs:474-490`) and then sends a kick IPI
(`kernel/src/arch/x86_64/apic.rs:256-260`), whose entry and handler run on the
woken CPU before its `hlt` returns (`kernel/src/arch/x86_64/idt/kick.rs:3-9`).
A CPU that waits in MWAIT on its doorbell's line (CPUID.01H:ECX[3]) is woken
by the store alone and takes no interrupt. The T14 enumerates it (`monitor`,
`kernel/cpuvuln/fixtures/t14/cpuinfo.txt:20`); whether the AMD bench does is
unread. `issues/idle-cpus-enter-the-c-states-cst-names.md` is which state the
wait enters; this file is what ends it. The doorbell shares a line with its
neighbours' published counters
(`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`),
so a monitor on it today would also wake on their writes.

**Measured** (`wt/toyos-scout-tlb` at `d4c2f8ebe`, `main`'s `177e72a9e` plus
the scout; https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198).
A user ping-pong between CPUs 4 and 5, each wake split from the poster's
send to the woken task running, p50 in TSC ticks per direction:

| | T14 (x2APIC, 2419 MHz) | AMD bench (xAPIC, 1796 MHz) |
|---|---|---|
| send | 189 | under one 18-tick quantum, taken as 0 |
| delivery, skew-free | 966 | 1062 |
| exit: the kick's entry and handler, to the instruction after `hlt` | 616 and 613 | 234 and 234 |
| whole wake | 12029 and 11854 | 5796 and 5724 |
| (send + delivery + exit) / wake, derived | 14.7 % and 14.9 % | 22.4 % and 22.6 % |

A CPU woken from MWAIT by the store skips all three segments but pays its own
wake from the store, which no run has timed and which delivery's C1 exit
stands in for here, so the share is an upper bound on what the doorbell
saves. Applied to that bound, the line fixed before the scout, the doorbell
dropped under 20 % of the wake, fires on the T14 and not on the AMD bench.

**Next: an MWAIT twin** beside a control arm, both timed by the same split in
the same session. The control arm is `main` plus the scout with the doorbell
on a line of its own, which is
`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`'s
change. The twin is the control arm with each idle CPU waiting in MWAIT on its
doorbell's line, hint C1 (EAX = 0), the state `hlt` enters, and the wake's
kick not sent. The two arms differ only in what ends the wait, which is what
the bound above covers. Its kill line, fixed here before it runs: the
doorbell is built ahead of `issues/idle-cpus-enter-the-c-states-cst-names.md`
only if, on a proving machine that enumerates MONITOR/MWAIT, the twin's whole
idle wake p50 is at least 20 % under the control arm's in both directions.
The T14's bound is under that line, so the twin runs on the AMD bench, and
reads CPUID.01H:ECX[3] there first; if the AMD bench does not enumerate it, no
proving machine can clear the line. Its clock control, fixed with the line:
each arm reads APERF and MPERF on CPUs 4 and 5 before and after the split,
and where the two arms' ratios of APERF's advance to MPERF's differ by more
than 1 % on either CPU, both arms' readings are void and decide nothing.

**Exit**: the twin's reading, posted, and an idle CPU on every proving machine
that enumerates MONITOR/MWAIT woken by its doorbell's store with no IPI. Where
the twin clears the line, that is built ahead of the C-states. Where it does
not, this file stays open behind
`issues/idle-cpus-enter-the-c-states-cst-names.md`: the 20 % prices an idle
path beside `hlt`, which that track builds anyway, and once an idle CPU waits
in MWAIT there, putting its monitor on the doorbell's line and sending no
kick adds no second path.
