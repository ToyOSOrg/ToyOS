---
status: open
kind: track
opened: 2026-09-28
---

# The kernel owns CPU performance state

The self-hosting bar (`issues/build/toyos-builds-itself.md`) is measured
under a fixed power envelope that must be read back for a whole build span.
Until this track, ToyOS left every register of it where firmware put it. The
kernel declares the envelope from the one CPU-state declaration
(`kernel/src/arch/x86_64/control_regs.rs`), and a `perf-state` claim reads it
back per CPU. The values the kernel programs are the bar's, not its own.

- **1 — the HWP request, declared and read back.** `IA32_PM_ENABLE`,
  `IA32_HWP_REQUEST` (min: the package's maximum-efficiency ratio; max: the
  CPU's highest performance; EPP 128), `IA32_HWP_REQUEST_PKG` and EPB 6,
  written whole on every CPU and asserted on each, refused by name on a CPU
  that lacks any of them. Read back through the `perf-state` claim, together
  with the turbo bit, `MSR_PKG_POWER_LIMIT`, the energy counter and package
  thermal status (`/system/bin/perfstate`). *Exit*: `perf_request` green in
  QEMU, which proves the refusal, and on the T14, which proves every CPU holds
  `0x80002a04` and reads it back.
- **2 — RAPL, declared.** PL1, PL2 and their windows through
  `MSR_PKG_POWER_LIMIT`, and the MMIO mirror in the host bridge's MCHBAR, at
  the bar's values; the peak limit beside them. A limit firmware locked (bit
  63) is refused by name, never worked around. *Exit*: the T14 reads both
  back at the bar's values.
- **3 — turbo, declared.** `IA32_MISC_ENABLE` bit 38 is read back and not
  written: its other bits are model-specific and firmware's, so declaring one
  bit needs the owner's ruling on writing that register whole. Linux's
  `platform_profile` has no ToyOS counterpart, and it is how the bar's firmware
  limits were chosen. *Exit*: the ruling, and the bit declared or recorded
  as firmware's.
- **4 — the sampler.** A program that reads the envelope every 60 s and at a
  span's start and end, and turns `MSR_PKG_ENERGY_STATUS` into the first
  60 s's package power and `IA32_PACKAGE_THERM_STATUS` into a temperature,
  which are the bar's validity conditions. *Exit*: one valid span on the T14.

**Not covered.** A hybrid CPU is refused: its HWP scale is not its ratio scale
and the declared minimum is a ratio. AMD's CPPC and AArch64 declare nothing;
the AArch64 kernel refuses the claim by name.

**What only the T14 proves.** No QEMU CPU enumerates HWP (TCG's `qemu64`, and
KVM, which reduces leaf 6 to `ARAT`), so every write and every read of these
registers runs only there. Under Linux on the T14 every CPU held
`IA32_HWP_REQUEST` `0x80002a04` with `IA32_HWP_CAPABILITIES` `0x010d182a` or
`0x010e182a`, and the package `IA32_HWP_REQUEST_PKG` `0x8000ff01`.
`MSR_PLATFORM_INFO` was not read there; its ratio 4 is inferred from Linux's
`cpuinfo_min_freq` of 400000 kHz, and ToyOS's boot line prints the register.
