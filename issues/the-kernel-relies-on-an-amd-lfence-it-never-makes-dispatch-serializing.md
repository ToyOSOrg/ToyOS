---
status: assigned
kind: defect
opened: 2026-11-11
---

# The kernel relies on an AMD LFENCE it never makes dispatch serializing

On AMD, LFENCE holds later instructions back only where it is dispatch
serializing: always on families 0Fh and 11h and where CPUID Fn8000_0021 EAX
bit 2 (`LFenceAlwaysSerializing`) is set (APM Vol. 3, LFENCE), and on
families 10h, 12h and 14h to 17h only once MSR C001_1029 bit 1 is set (AMD
70586, "Software Techniques for Managing Speculation on AMD Processors",
G-2). The kernel neither sets that MSR bit nor reads the CPUID one, so on a
CPU that does not serialize by itself an LFENCE orders loads and nothing
else.

`cpu::rdtsc_ordered`, `RDTSCP; LFENCE`, relies on its LFENCE holding later
instructions back: the kernel's own clock read (`cpu::counter_ordered` inside
the clock page's latch), and in `arch/x86_64/clocksource.rs` the demotion's
two readings, `floor_here`, `stamp_now`, the AP start's trip, `check`, and the
brackets around each HPET and PM timer sample.

Nothing else does. `cpu::wrmsr_fence`, `MFENCE; LFENCE` before an x2APIC
`WRMSR`, is ordered by its MFENCE, which is serializing on AMD (APM Vol. 3,
MFENCE), and Intel's LFENCE always is. A process's clock read
(`toyos_abi::arch::counter`) is `RDTSCP` with nothing after it. The guest
timing binaries `syscall_cost` and `wake_storm_cost` take `LFENCE; RDTSC` for
their own measurements.

The AMD bench's family is among those the MSR bit decides, and no boot reads
the bit as firmware leaves it.

**Exit**: #1243 sets C001_1029 bit 1 on every CPU whose decision names it,
before the kernel's first LFENCE use, and reads it back in the boot's line.

**Owner**: harden, in #1243 (`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`).
