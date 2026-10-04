---
status: open
kind: track
opened: 2026-09-28
---

# The kernel owns CPU performance state

The self-hosting bar is measured under a fixed power envelope that must be
read back for a whole build span. The kernel declares the envelope from the
one CPU-state declaration (`kernel/src/arch/x86_64/control_regs.rs`) and
`SYS_COUNTERS` reads it back per CPU, under `Rights::TRACE`.

**A register reaches a read only once boot has proven it**: the kernel has no
`rdmsr` fault fixup, so a register a read is the first to touch is a kernel
`#GP` a holder of the right can cause. Each register is enumerated by CPUID
or read at boot by the declaration's own check before a read can reach it.

Declared and read back: the HWP request, its package's and the energy/
performance bias, on every CPU that has all of them
(`toyos_cpuvuln::hwp`). Still to build:

- **RAPL**: PL1, PL2 and their windows through `MSR_PKG_POWER_LIMIT` and the
  MMIO mirror in the host bridge's MCHBAR, at the bar's values; a limit
  firmware locked (bit 63) is refused by name. Neither register is
  enumerated by CPUID. *Exit*: the T14 reads both back at the bar's values.
- **Turbo**: `IA32_MISC_ENABLE` bit 38 is firmware's. Its other bits are
  model-specific, so declaring the one bit needs the owner's ruling on
  writing that register whole. *Exit*: the ruling, and the bit declared or
  recorded as firmware's.
- **The sampler**: a program that reads the envelope every 60 s and at a
  span's start and end, with package power from `MSR_PKG_ENERGY_STATUS` and
  temperature from `IA32_PACKAGE_THERM_STATUS` and `MSR_TEMPERATURE_TARGET`,
  the bar's validity conditions. The energy counter is a side channel
  (CVE-2020-8694), under `Rights::TRACE` like the rest. *Exit*: one valid
  span on the T14.

Not covered: hybrid Intel and AMD CPPC,
`issues/the-perf-state-declaration-refuses-hybrid-intel-and-amd-cppc.md`;
AArch64 declares nothing. No QEMU CPU enumerates HWP, so every write and read
of these registers runs only on the T14.

Owner: the orchestrator, which holds the T14.
