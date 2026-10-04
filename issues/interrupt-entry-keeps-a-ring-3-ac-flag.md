---
status: open
kind: defect
opened: 2026-09-27
---

# Interrupt entry keeps a Ring 3 thread's AC flag

SMAP binds a Ring 0 access only while `RFLAGS.AC` is clear, and a Ring 3 thread
can set `AC` with `popf` (`CR0.AM` is clear, so it costs the thread nothing).
The syscall entry clears it through `IA32_FMASK`
(`kernel/src/arch/x86_64/syscall.rs`), but an interrupt or trap gate does not:
Intel SDM Vol. 3A §6.12.1.3 names TF, VM, RF and NT, and IF for an interrupt
gate. `arch::entry::ring3_naked_asm` prepends only `cld`, and
`kernel/src/arch/x86_64/control_regs.rs` says the boot `clac` is the only one
the kernel needs.

So an interrupt or exception taken from a Ring 3 thread that set `AC` runs its
handler with SMAP off: a kernel bug there that touches a user address reads or
writes it silently.

**Evidence:** read from the code and the SDM; no test stages it.

**Exit condition:** every Ring 0 entry from Ring 3 runs with `AC` clear, gated by
a guest program that sets `AC` and a handler that must fault on a user address.
