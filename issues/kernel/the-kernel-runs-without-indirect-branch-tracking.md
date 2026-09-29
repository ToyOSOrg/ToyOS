---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel runs without indirect branch tracking

Nothing in the build asks for `endbr64` and nothing sets `S_CET.ENDBR_EN`, so
an indirect call or jump in the kernel may land anywhere in its text. Linux at
`Ubuntu-6.8.0-142.142` leaves `X86_KERNEL_IBT` unset
(`debian.master/config/annotations:834`); the kernel takes it where the CPU
enumerates IBT.

**Exit**: a gate finds `endbr64` at every IDT and `IA32_LSTAR` target and no
`notrack` in `kernel.elf`; on the T14, the one proving machine known to
enumerate IBT, a call to a function without one is `#CP`. **Mutation**:
`syscall_entry`'s `endbr64` gone; `ENDBR_EN` clear. **Oracle**: the T14's CPU.
