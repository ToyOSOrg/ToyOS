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
G-2). The kernel
neither sets that MSR bit nor reads the CPUID one, so on a CPU that does not
serialize by itself an LFENCE orders loads and nothing else.

Two kernel sequences rely on it holding later instructions back:

- `cpu::rdtsc_ordered`, `RDTSCP; LFENCE`: the kernel's own clock read
  (`cpu::counter_ordered` inside the clock page's latch), and in
  `arch/x86_64/clocksource.rs` the demotion's two readings, `floor_here`,
  `stamp_now`, the AP start's trip, `check`, and the brackets around each
  HPET and PM timer sample.
- `cpu::wrmsr_fence`, `MFENCE; LFENCE` before an x2APIC `WRMSR` (SDM Vol. 3A
  §13.12.3).

A process's clock read (`toyos_abi::arch::counter`, `RDTSCP` with nothing
after it) relies on neither. The guest timing binaries `syscall_cost` and
`wake_storm_cost` take `LFENCE; RDTSC` for their own measurements.

No boot records whether the AMD bench's LFENCE serializes.

**Exit**: #1243 sets C001_1029 bit 1 on every CPU whose decision names it,
before the kernel's first LFENCE use, and reads it back in the boot's line.

**Owner**: harden, in #1243 (`issues/a-pure-function-decides-a-cpus-speculation-mitigations-from-its-vendors-guidance.md`).
