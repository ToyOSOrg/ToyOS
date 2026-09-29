---
status: open
kind: tooling
opened: 2026-09-29
---

# No gate decodes `kernel.elf`'s instructions

Five stages each owe a gate over `kernel.elf`'s instructions:

- `issues/kernel/no-entry-or-switch-clears-the-bhb-fills-the-rsb-or-issues-an-ibpb.md`
- `issues/kernel/indirect-branches-and-returns-run-without-thunks.md`
- `issues/kernel/kernel-functions-return-with-their-used-registers-intact.md`
- `issues/kernel/the-kernel-runs-without-indirect-branch-tracking.md`
- `issues/kernel/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`

The one gate that reads `kernel.elf` today matches bytes at a symbol
(`judge_entry_window`, `src/build.rs:1641`), which cannot tell an opcode from a
displacement or an immediate. The five share one x86-64 decoder, in Rust in the
tree and run by the host gate, with no external disassembler: it lands before
any of them, and each adds its own check over it rather than a decoder of its
own.

**Exit**: the decoder walks every function `kernel.elf`'s `.symtab` names, from
its address to its size, and a walk that decodes a byte it does not know or
ends anywhere but the function's end is refused. **Mutation**: an instruction
length misread by one, a SIB byte dropped after a ModRM r/m of 100b, reds the
walk over the shipped kernel. **Oracle**: SDM Vol. 2's opcode maps, Appendix A.
