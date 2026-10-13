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
loop. Nor does the AMD bench's own reference: it enumerates MONITOR/MWAIT
unless firmware disables them, gives the monitor line as 64 bytes, and has
MWAIT support one C1 sub-state (PPR 55922 rev. 3.06, `CPUID_Fn00000001_ECX`
bit 3, `CPUID_Fn00000005`). So the stage is backed on Intel by §11.10.4 and
§11.10.6.4. On AMD it follows the firmware's `_CST` (the ground of
`issues/idle-cpus-enter-the-c-states-cst-names.md`): a C1 the AMD bench's
`_CST` enters through Functional Fixed Hardware is the firmware declaring C1
entered by MWAIT, and that `_CST` is unread. A CPU whose vendor's erratum says
a store may not wake MWAIT keeps the kick: Intel's APL30, "A Store
Instruction May Not Wake Up MWAIT", names Goldmont (family 6 model 5Ch), and
its workaround is that "Software needs to use interrupts to wake processors
from MWAIT-induced sleep states" (specification update 334820-010). The T14's
lists no MONITOR or MWAIT erratum (631123-034). The T14 enumerates
MONITOR/MWAIT (`monitor`, `kernel/cpuvuln/fixtures/t14/cpuinfo.txt:20`);
whether the AMD bench does is unread. `issues/idle-cpus-enter-the-c-states-cst-names.md` is which state the
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

**Exit**: an idle CPU on every Intel proving machine that enumerates
MONITOR/MWAIT woken by its doorbell's store with no IPI, and the split above
read again on the T14 after it; the kick kept on a CPU a vendor erratum names,
by a verdict that is a pure function of CPUID's vendor, family and model and
is host-tested, red where Goldmont is woken by the store alone or the T14 is
kicked; and the AMD bench's `_CST` read, the doorbell taken there and the
split read again where its C1 is FFH MWAIT, and `hlt` with the kick kept where
it is not.
