---
status: open
kind: defect
opened: 2026-11-11
severity: [security, kernel-from-userland]
---

# Ring 3 replaces the kernel's GS base with a segment load

The kernel runs no `swapgs`: GS holds this CPU's per-CPU block from boot on,
and the `SYSCALL` entry's first instructions address it through `gs:` while
still on the user stack (`kernel/src/arch/x86_64/syscall.rs`). `CR4.FSGSBASE`
is forbidden so that Ring 3 cannot write the base with `WRGSBASE`
(`issues/fsgsbase-is-taken.md`), but a segment load writes it too: a `MOV` to
GS with a non-null selector Ring 3 may load — the GDT's DPL-3 user data or
user code descriptor — loads that descriptor, base included (SDM Vol. 2B, MOV,
Operation: "IF DS, ES, FS, or GS is loaded with non-NULL selector ...
SegmentRegister := segment descriptor"), and both of those bases are zero.

So two user instructions, `mov ax, 0x1b; mov gs, ax`, before a `syscall` aim
the entry's first store at linear address `0x10`-odd, below anything a space
maps. The page fault is taken at CPL 0 on the user's own stack pointer, since
`SYSCALL` switches no stack and `#PF` has no IST entry
(`kernel/src/arch/x86_64/idt/mod.rs`): with the user stack there, the push
faults again and the machine ends in `#DF`; with a stack pointer the program
chose inside the kernel half, the processor writes the exception frame there
first. Every handler after it addresses `gs:` with the same zero base. Found by
reading, while deciding whether the switch may keep a thread's FS base instead
of reading it back (it may not, for the same reason); not reproduced.

**Exit**: a guest test, run on the T14 and the AMD bench, whose child loads GS
with each selector Ring 3 can name and then makes a system call, and the kernel
survives and ends the child; its negative control is the kernel without the
fix. The fix is an entry that does not trust the GS base it finds, the owner's
to choose; `issues/fsgsbase-is-taken.md` weighs the `swapgs` one. A GDT without
a loadable Ring 3 data descriptor is not one: `IRETQ` to Ring 3 needs SS to name
a writable DPL-3 data segment.
