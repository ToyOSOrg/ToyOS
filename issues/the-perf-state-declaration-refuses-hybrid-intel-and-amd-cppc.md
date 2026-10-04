---
status: open
kind: defect
opened: 2026-09-29
---

# The perf-state declaration refuses hybrid Intel and AMD CPPC

Every Intel client CPU since Alder Lake is hybrid, and AMD CPUs name their
performance controls through CPPC, not HWP. On both the kernel declares no
performance request, firmware's values stand, and `SYS_COUNTERS` carries no
power envelope.

The refusals are `toyos_cpuvuln::hwp`'s (`toyos-cpuvuln/src/hwp.rs`):
- `HwpRefusal::Hybrid`, for CPUID.(7,0):EDX[15]: the declared minimum is a
  ratio (`MSR_PLATFORM_INFO[47:40]`), and a hybrid CPU's HWP scale is not its
  ratio scale.
- `HwpRefusal::NoHwp`, for CPUID.6:EAX[7] clear, which is where an AMD CPU
  with CPPC is refused; its controls are not read.
- `HwpRefusal::NotIntel`, for HWP on any vendor but Intel, because the
  minimum comes from `MSR_PLATFORM_INFO`, which is Intel's.

`control_regs::hwp_declared` logs the refusal once and declares nothing on
any CPU.

Owner: the orchestrator. **Exit**: on a hybrid Intel CPU each core type's
request is declared on that core's own HWP scale; on an AMD CPU with CPPC,
`MSR_AMD_CPPC_ENABLE` and `MSR_AMD_CPPC_REQ` are declared from its
`MSR_AMD_CPPC_CAP1` and asserted on every CPU; and on one machine of each
kind the `counters` metal row holds the envelope it reads back to the
declaration.
