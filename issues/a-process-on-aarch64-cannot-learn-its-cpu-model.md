---
status: open
kind: track
opened: 2026-10-10
---

# A process on AArch64 cannot learn its CPU model

On x86-64 a process reads its CPU's vendor and brand string with CPUID,
which the kernel does not fault, and ToyOSOrg/sysinfo's ToyOS backend
answers `Cpu::brand` and `Cpu::vendor_id` from it. On AArch64 the model is
`MIDR_EL1`, which traps from EL0, and an unserved EL0 sync exception ends the
process (`kernel/src/arch/aarch64/trap.rs`); the kernel publishes it nowhere.
So the backend answers empty strings there, and `toyfetch` says "model not
reported".

Built: nothing. The answer is ambient, as the `SYS_SYSINFO` header is: a
CPU's identity authorizes nothing.

## Exit condition

A process on AArch64 reads each CPU's `MIDR_EL1` (implementer, part,
variant, revision) without a capability, the fork's AArch64 arm answers
`vendor_id` and `brand` from it, and `toyfetch` on the `virt` machine prints
a model.
