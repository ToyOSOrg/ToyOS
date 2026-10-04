---
status: open
kind: tooling
opened: 2026-09-29
---

# No gate decodes `kernel.elf`'s instructions

Five stages each owe a gate over `kernel.elf`'s instructions:

- `issues/no-entry-or-switch-clears-the-bhb-fills-the-rsb-or-issues-an-ibpb.md`
- `issues/indirect-branches-and-returns-run-without-thunks.md`
- `issues/kernel-functions-return-with-their-used-registers-intact.md`
- `issues/the-kernel-runs-without-indirect-branch-tracking.md`
- `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`

The five share one x86-64 decoder, in Rust in the tree and run by the host
gate, with no external disassembler: it lands before any of them, and each adds
its own check over it rather than a decoder of its own. It takes its functions
from `toyos_symbols::locate` over `toyos_elf` (`src/build.rs:1575-1598`), not
from a second ELF reader. `global_asm!` emits no `.type` or `.size`, so code
such as the AP trampoline (`kernel/src/arch/x86_64/smp.rs:396`), in `.text`
with a `jmp far [mem]` and a `retf`, is covered by no sized function.

**Exit**: the decoder walks every function `kernel.elf`'s `.symtab` names, from
its address to its size, and a walk that decodes a byte it does not know or
ends anywhere but the function's end is refused; so is any byte of an
executable section that no walked function covers, apart from a region named by
symbol and decoded in its own mode, as the trampoline's 16-, 32- and 64-bit
code is. **Mutation**, each red over the shipped kernel: an instruction length
misread by one, a SIB byte dropped after a ModRM r/m of 100b;
`global_asm!("1: jmp rax")` in `.text`. **Oracle**: SDM Vol. 2's opcode maps,
Appendix A.
