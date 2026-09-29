---
status: open
kind: defect
opened: 2026-09-29
---

# The perf-state declaration refuses hybrid Intel and AMD CPPC

Every Intel client CPU since Alder Lake is hybrid, and AMD CPUs name their
performance controls through CPPC, not HWP. On both, the kernel declares no
performance request, firmware's values stand, and a `perf-state` claim is
refused `NotFound`.

The refusal sites, all in `toyos-perfstate/src/lib.rs`'s `refusal`:
- `Refusal::Hybrid`, for CPUID.07H:EDX[15]. The reason it gives is that the
  declared minimum is a ratio (`MSR_PLATFORM_INFO[47:40]`), and a hybrid
  CPU's HWP scale is not its ratio scale.
- `Refusal::NoHwp`, for CPUID.06H:EAX[7] clear. AMD does not set that bit, so
  this is where an AMD CPU with CPPC is refused. The CPPC controls are not
  read at all.
- `Refusal::NotIntel`, for HWP on any vendor but `GenuineIntel`, because the
  minimum comes from `MSR_PLATFORM_INFO`, which is Intel's.

`kernel/src/arch/x86_64/control_regs.rs`'s `hwp_declared` logs the refusal once
and declares nothing on any CPU.

**Exit**: on a hybrid Intel CPU, each core type's request is declared on that
core's own HWP scale, and the minimum is not read as a ratio. On an AMD CPU
with CPPC, `MSR_AMD_CPPC_ENABLE` and `MSR_AMD_CPPC_REQ` are declared from its
`MSR_AMD_CPPC_CAP1` and asserted on every CPU. On each machine the claim reads
the declaration back, and `perf_request`'s metal row passes on one machine of
each kind.
