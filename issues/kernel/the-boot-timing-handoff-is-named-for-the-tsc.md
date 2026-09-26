---
status: open
kind: defect
opened: 2026-09-26
---

# The boot-timing handoff is named for the TSC

`toyos-abi/src/boot.rs`'s `KernelArgs` carries `loader_entry_tsc` and
`loader_handoff_tsc`, and `kernel/src/main.rs`'s `report_power_on` reports
"the TSC went backwards". On AArch64 the loader fills both from `CNTVCT_EL0`,
the generic timer's count, so the ABI and the message name a counter the
machine does not have.

Owned by stage 4 of `issues/kernel/toyos-runs-on-arm64.md`, which makes the
generic timer the clock. `KernelArgs` is the ABI, so the rename lands with
that stage's other ABI work.

**Exit condition**: the two fields and the report name the CPU's counter
(`cpu::counter`) rather than the TSC, and both architectures' loaders fill
them.
