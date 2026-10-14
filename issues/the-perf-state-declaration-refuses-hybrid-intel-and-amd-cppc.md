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

The refusals are `kernel::cpu::hwp`'s (`kernel/pure/cpu.rs`):
- `HwpRefusal::Hybrid`, for CPUID.(07H,0):EDX[15]. No bench ToyOS has is
  hybrid, so the declaration cannot be measured on one, and the owner ruled
  no design for a machine that cannot be measured (#915, ruling B).
- `HwpRefusal::NoHwp`, for CPUID.06H:EAX[7] clear, which is where an AMD CPU
  with CPPC is refused; its controls are not read.
- `HwpRefusal::NotIntel`, for HWP's bits on any vendor but Intel, where SDM
  Vol. 1 §21.1.1 gives them no meaning.

`control_regs::hwp_declared` logs the refusal once and declares nothing on
any CPU.

Owner: the orchestrator. **Exit**: on a hybrid Intel CPU each core type's
request is declared on that core's own HWP scale; on an AMD CPU with CPPC,
`MSR_AMD_CPPC_ENABLE` and `MSR_AMD_CPPC_REQ` are declared from its
`MSR_AMD_CPPC_CAP1` and asserted on every CPU; and on one machine of each
kind the `counters` metal row holds the envelope it reads back to the
declaration.
