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
by the store alone and takes no interrupt. The SDM names the IPI a halted CPU
needs as a delay in serving new work and has an OS consider MONITOR/MWAIT in
place of `hlt` in its C1 idle loop (325462-093, Vol. 3A §11.10.4,
§11.10.6.4), and Intel's optimization manual names the C1 loop's
responsiveness as what that buys (248966-051, Vol. 1 §11.4). AMD's APM
documents MWAIT as a wait for another processor's store to the range MONITOR
set up, sized by CPUID 0000_0005h (24594 rev. 3.36, Vol. 3, MONITOR and
MWAIT), and neither it, its Vol. 2 (24593 rev. 3.42) nor its optimization
guide for family 17h models 30h and greater (56305 rev. 3.02) names an idle
loop. AMD's own Linux commit aebef63cf7ff (v6.0, by AMD's Wyes Karny) does:
"AMD Family 17h onwards supports MWAIT as well as MWAIT C1. And it is
preferable to use MWAIT as the default idle state on these systems, as it has
lower exit latencies", and at v7.2 Linux's `prefer_mwait_c1_over_halt` checks
no vendor (`arch/x86/kernel/process.c`). So the stage is backed on
both benches: on Intel by §11.10.4 and §11.10.6.4, and on AMD from family 17h,
the AMD bench's, by aebef63cf7ff. A CPU whose MONITOR loses wakes keeps the
kick, as Linux keeps it: Intel's c9a4b55431e5 (v6.13, by Len Brown) adds Lunar
Lake to Goldmont in `X86_BUG_MONITOR` because "MONITOR wakeups on Lunar Lake
processors can be lost, resulting in significant user-visible delays", "so
that wake_up_idle_cpu() always sends an IPI"; at v7.2 such a CPU's MWAIT idle
sets no polling flag, so every wake of it is an IPI
(`arch/x86/include/asm/mwait.h`), and `prefer_mwait_c1_over_halt` gives it
`hlt`. The T14 enumerates MONITOR/MWAIT (`monitor`,
`kernel/cpuvuln/fixtures/t14/cpuinfo.txt:20`); whether the AMD bench does is
unread. `issues/idle-cpus-enter-the-c-states-cst-names.md` is which state the
wait enters; this file is what ends it. The doorbell shares a line with its
neighbours' published counters
(`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`),
so a monitor on it today would also wake on their writes; the SDM has
monitored data padded to the largest monitor line size CPUID.05H reports
(§11.10.5).

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
saves.

**Exit**: an idle CPU on every proving machine that enumerates MONITOR/MWAIT
woken by its doorbell's store with no IPI, a CPU whose MONITOR loses wakes
still kicked, and the split above read again on both benches after it.
