---
status: open
kind: defect
severity: [security]
opened: 2026-11-12
---

# No AArch64 program runs with speculative store bypass disabled

A program whose row in the system manifest asks for isolated speculation
runs with SSBD on x86-64 where its CPU's decision names it
(`kernel::mitigation::OnRequest`), and on AArch64 with nothing: no AArch64
decision exists, and the switch sets nothing for it. Which control Arm
prescribes, `FEAT_SSBS`'s `PSTATE.SSBS` or the firmware's
`SMCCC_ARCH_WORKAROUND_2` on a CPU without it, is for that decision to
establish from Arm's documents. No proving machine is AArch64.

**Exit**: an AArch64 decision names what Arm prescribes for the CPU, and a
program that asked runs with store bypass disabled by it, as does its
child, while one that did not runs without. **Mutation**: no inheritance.
**Oracle**: the CPU's own state, read by the kernel on the program's
behalf.
