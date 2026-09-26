---
status: open
kind: defect
opened: 2026-09-26
---

# The crash evidence records x86 fault registers, in slots an AArch64 CPU aliases

`kernel/src/panic.rs`'s `Evidence` stores `apic`, `rip` and `cr2`, and
`record_fault` takes them by those names: the x86 APIC id, the faulting
instruction and x86's fault address. On AArch64 the same three are the MPIDR
affinity, `ELR_EL1` and `FAR_EL1`, and the report would print them under x86
labels.

The slot is `FIRST[cpu::hardware_id() & (SLOTS - 1)]`, `SLOTS = 64`, as is
`PANIC_DEPTH`'s. An APIC id below 64 is one slot per CPU. AArch64's
`hardware_id` is MPIDR's `Aff3:Aff2:Aff1:Aff0`, so a second cluster's CPU 0
(`Aff1 = 1`, `0x100`) lands on slot 0 beside the first CPU of all. QEMU `virt`
numbers CPUs 16 and up that way, so two CPUs share one record and one panic
depth, which the `apic` field was left unmasked to make visible and not to
prevent.

Owned by stage 5 of `issues/kernel/toyos-runs-on-arm64.md`, the first stage
that starts a second CPU.

**Exit condition**: the evidence names the fault by role (the CPU, the
faulting PC, the faulting address) and each architecture's report labels
them; the slot is the CPU's dense index, not its hardware id masked, shown
by a test that panics a CPU whose `Aff1` is not zero beside CPU 0 and reads
both records.
