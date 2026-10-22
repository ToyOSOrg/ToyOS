---
status: open
kind: defect
opened: 2026-10-22
---

# osbench builds for no AArch64 ToyOS

`tests/guest/osbench` names the CPU a thread runs on, and the cores CPUs
share, to stage and check its placement benchmarks. Linux answers both itself
(`sched_getcpu`, sysfs topology). ToyOS has no call for either, so osbench
reads them with x86-64's CPUID (`src/arch/x86_64.rs`). AArch64 offers no EL0
equivalent: `MPIDR_EL1` is not readable at EL0. So `src/arch/mod.rs` refuses
`aarch64-unknown-toyos` with a `compile_error!`, and no AArch64 machine can be
compared.

**Exit**: osbench builds and runs its placement benchmarks on an AArch64 ToyOS
guest, either from a ToyOS call that names a thread's CPU and its core, or from
an EL0-readable register the kernel sets per CPU.
