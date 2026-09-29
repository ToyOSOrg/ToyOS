---
status: open
kind: track
opened: 2026-09-28
---

# The kernel owns CPU performance state

The self-hosting bar is measured under a fixed power envelope that must be
read back for a whole build span.
The kernel declares the envelope from the one CPU-state declaration
(`kernel/src/arch/x86_64/control_regs.rs`), and a `perf-state` claim reads it
back per CPU.

**A register reaches a claim only once boot has proven it.** The kernel has no
`rdmsr` fault fixup, so a register a userland read is the first to touch is a
kernel `#GP` any holder of the claim can cause. Each register a stage adds is
enumerated by CPUID or read at boot under the declaration's proof
(`control_regs::HwpDeclared`) before any read of the claim can reach it.

- **1 — the HWP request, declared and read back.** `IA32_HWP_INTERRUPT` 0
  where CPUID enumerates it, `IA32_PM_ENABLE`, `IA32_HWP_REQUEST` (min: the
  package's maximum-efficiency ratio; max: the CPU's highest performance; EPP
  128), `IA32_HWP_REQUEST_PKG` and EPB 6, written whole on every CPU and
  asserted on each, refused by name on a CPU that lacks any of them or is not
  DisplayFamily 06H (`MSR_PLATFORM_INFO`'s family). Read back through the
  `perf-state` claim, together with the turbo bit and package thermal status
  (`/system/bin/perfstate`). *Exit*: in QEMU, `perf_request` (the refusal) and
  `perf_state_silent_cpu` (a CPU that never answers is refused `Io` by name);
  on the T14, **owed**, `perf_request`'s metal row: on `testcases` every CPU
  logs `control_regs: cpuN pm_enable=1 hwp_request=0x80002a04
  hwp_request_pkg=0x8000ff01 epb=6` and `test_rs_perf_state` exits 0; on
  `perfdiverge` the page after the reset carries the panic `control_regs:
  cpu1 holds hwp_request=0x80002a05, the declaration is 0x80002a04`.
  No test launches `/system/bin/perfstate`, so its row's `devices` is
  unmeasured.
- **2 — RAPL, declared.** PL1, PL2 and their windows through
  `MSR_PKG_POWER_LIMIT`, and the MMIO mirror in the host bridge's MCHBAR, at
  the bar's values; the peak limit beside them. A limit firmware locked (bit
  63) is refused by name, never worked around. `MSR_RAPL_POWER_UNIT` and
  `MSR_PKG_POWER_LIMIT` are enumerated by no CPUID bit, so each is read at
  boot before the claim answers it. *Exit*: the T14 reads both back at the
  bar's values.
- **3 — turbo, declared.** `IA32_MISC_ENABLE` bit 38 is read back and not
  written: its other bits are model-specific and firmware's, so declaring one
  bit needs the owner's ruling on writing that register whole. Linux's
  `platform_profile` has no ToyOS counterpart, and it is how the bar's firmware
  limits were chosen. *Exit*: the ruling, and the bit declared or recorded
  as firmware's.
- **4 — the sampler.** A program that reads the envelope every 60 s and at a
  span's start and end, and turns `MSR_PKG_ENERGY_STATUS` into the first
  60 s's package power and `IA32_PACKAGE_THERM_STATUS` with
  `MSR_TEMPERATURE_TARGET` into a temperature, which are the bar's validity
  conditions. The energy counter and the temperature target are enumerated by
  no CPUID bit, so each is read at boot before the claim answers it; and the
  energy counter is a side channel (CVE-2020-8694), so it reaches only a row
  the owner rules on, never the `perfstate` row any session can launch.
  *Exit*: one valid span on the T14.

**Not covered.** A hybrid CPU is refused: its HWP scale is not its ratio scale
and the declared minimum is a ratio. AMD's CPPC and AArch64 declare nothing;
the AArch64 kernel refuses the claim by name.

**What only the T14 proves.** No QEMU CPU enumerates HWP (TCG's `qemu64`, and
KVM, which reduces leaf 6 to `ARAT`), so every write and every read of these
registers runs only there. Under Linux on the T14 (#568's samples, not merged;
the metal row re-measures them) every CPU held
`IA32_HWP_REQUEST` `0x80002a04` with `IA32_HWP_CAPABILITIES` `0x010d182a` or
`0x010e182a`, and the package `IA32_HWP_REQUEST_PKG` `0x8000ff01`.
`MSR_PLATFORM_INFO` was not read there; its ratio 4 is inferred from Linux's
`cpuinfo_min_freq` of 400000 kHz, and ToyOS's boot line prints the register.
