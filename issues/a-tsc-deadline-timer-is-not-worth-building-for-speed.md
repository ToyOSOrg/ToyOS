---
status: none
kind: rejected
opened: 2026-10-12
---

# A TSC-deadline timer is not worth building for speed

Every scheduler pass ends by handing its timer an absolute deadline
(`kernel/pure/sched/cpu.rs:1912-1918`), and the kernel turns it back into a
span from now (`kernel/src/hw.rs:47-51`) that `apic::arm_one_shot` converts to
LAPIC ticks by the boot's calibration against the TSC-derived clock
(`kernel/src/arch/x86_64/apic.rs:307-336`) and writes as three APIC register
writes, `TIMER_DIVIDE`, `LVT_TIMER` and `TIMER_INIT` (`apic.rs:355-368`):
MMIO stores under xAPIC, `wrmsr` under x2APIC (`apic.rs:49-64`). TSC-deadline
mode (CPUID.01H:ECX[24]) would take the absolute deadline as one
`IA32_TSC_DEADLINE` write.

**Measured** (`wt/toyos-scout-tlb` at `d4c2f8ebe`, `main`'s `177e72a9e` plus
the scout; https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198),
p50 in TSC ticks inside an idle wake of 12029 and 11854 ticks on the T14 and
5796 and 5724 on the AMD bench:

- The re-arm: 518 and 522 on the T14 (x2APIC, 214 ns), 4.3 % and 4.4 % of the
  wake; 18 on the AMD bench (xAPIC), its TSC's one-quantum floor, so at most
  about 20 ns and 0.3 %.
- The timer stop on idle entry: 138 on the T14 (57 ns, its 203 less one
  65-tick record), and below the AMD bench's resolution.

No kill line was fixed for the re-arm before the run, so this is a judgement
on the reading, not a condition firing. Removing the re-arm and the stop
whole would save at most 5.5 % of an idle wake on the T14 (656 of 12029,
derived) and nothing the AMD bench's TSC resolves, and a TSC deadline still
writes once to arm and once to stop. That is under the lines the other two
changes that take instructions off a hot path were held to, 15 % for the
syscall's FP save and `lock` prefixes and 20 % for the MWAIT doorbell, so
it does not earn an arm path of its own as a speed feature. Whether either bench enumerates CPUID.01H:ECX[24] was not
read. A case for a TSC deadline that is not speed, such as the lateness the
LAPIC count's calibration adds, is a defect of its own with its reading.
