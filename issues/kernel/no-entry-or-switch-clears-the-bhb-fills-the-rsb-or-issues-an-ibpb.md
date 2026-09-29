---
status: open
kind: defect
opened: 2026-09-29
---

# No entry or switch clears the BHB, fills the RSB or issues an IBPB

Nothing in `kernel/src` clears branch history, fills the return stack buffer
or issues an indirect branch prediction barrier. Linux at
`Ubuntu-6.8.0-142.142` runs `clear_bhb_loop` on entry where its decision
selects the BHI loop (`arch/x86/entry/entry_64.S:1534`),
`__FILL_RETURN_BUFFER` on a switch where it selects RSB filling
(`arch/x86/include/asm/nospec-branch.h:151`), and an IBPB on a switch between
address spaces one of which asked for it (`arch/x86/mm/tlb.c:384`). In ToyOS
the ask is the flag of
`issues/kernel/no-program-runs-with-speculative-store-bypass-disabled.md`.

**Exit**: a gate over `kernel.elf`, on the decoder of
`issues/build/no-gate-decodes-kernel-elfs-instructions.md`, matches it against
both of Linux's sequences; on
every proving machine probes count what
`issues/kernel/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`
selects for it: a clear per syscall where it selects the loop, a fill per
switch where it selects filling, and two IBPBs over B, C, idle, B, A, idle, A,
B. **Mutation**: either sequence edited, the clear's condition inverted, no
fill, the IBPB decided by the previous root. **Oracle**: Linux's sequences.
