---
status: open
kind: defect
opened: 2026-09-29
---

# SPEC_CTRL and GDS stay as firmware left them

Nothing in `kernel/src` reads or writes `IA32_SPEC_CTRL` (0x48) or
`IA32_MCU_OPT_CTRL` (0x123), so every CPU runs under the speculation controls
firmware left, and an Intel part affected by Gather Data Sampling runs
unmitigated if firmware left `GDS_MITG_DIS` set. Linux at
`Ubuntu-6.8.0-142.142` writes both as its decision selects
(`arch/x86/kernel/cpu/bugs.c:89,777`).

**Exit**: on every proving machine each CPU's 0x48 holds what
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`
selects for it, on the T14 Linux's read less SSBD; on the T14, the one proving
machine with GDS, a probe sets `GDS_MITG_DIS` and reads it clear after the
kernel's write. **Mutation**: the clear deleted. **Oracle**: Linux's reads on
each machine.
