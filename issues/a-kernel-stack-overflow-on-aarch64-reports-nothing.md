---
status: open
kind: defect
opened: 2026-11-11
---

# A kernel stack overflow on AArch64 reports nothing

Every kernel stack has an unmapped page under it (`kernel/src/mm/stack.rs`,
and the boot stack's in `kernel/src/mm/image.rs`), on both architectures. On
x86-64 running into one is a `#DF` on IST1, whose report names the stack. On
AArch64 the vectors run on the interrupted `SP_EL1`
(`kernel/src/arch/aarch64/trap.rs`, `report_fault_stack`), so the abort the
guard raises is taken on the stack that just ran out, faults again on the same
page, and the CPU takes that vector for good: the overflow no longer writes
into a neighbour, and it says nothing. `stack-overflow-task`, `-idle` and
`-boot` stage it and no AArch64 row runs them.

`aarch64-unknown-none-softfloat` also emits no stack probes, so a frame larger
than the guard can step over it; `x86_64-unknown-none` probes inline.

**Exit**: an EL1 entry that finds `sp` outside the running stack's bounds
moves to a per-CPU overflow stack and reports the stack by name, as x86-64's
`#DF` does; `a_kernel_stack_overflow_names_its_stack` runs its rows on `virt`;
and either the kernel's frames are probed or a gate bounds the largest.
