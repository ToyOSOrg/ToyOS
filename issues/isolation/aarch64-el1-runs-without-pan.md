---
status: open
kind: defect
opened: 2026-09-29
---

# AArch64's EL1 runs without PAN

`kernel/src/arch/aarch64/control_regs.rs`'s `SCTLR` leaves `SPAN` set, so an
exception taken to EL1 leaves `PSTATE.PAN` as it was, and nothing writes
`PSTATE.PAN`: EL1 can load and store through any EL0 mapping. x86-64 runs
with SMAP (`kernel/src/arch/x86_64/control_regs.rs`), so a kernel bug that
dereferences a user pointer directly faults there and not here.

The kernel reaches user memory through the direct map (`kernel/src/user_ptr.rs`)
and never through a user address, so PAN costs no
unprivileged-access instruction; what it needs is FEAT_PAN in the
declaration's check and `SPAN` clear.

**Exit condition**: `SCTLR_EL1.SPAN` is clear and `PSTATE.PAN` set at EL1 on
every CPU, `control_regs::check` refuses a CPU without FEAT_PAN, and a guest
test whose kernel reads a user address directly under `test-actuators`
takes a permission fault.
