---
status: open
kind: track
opened: 2026-09-29
---

# Kernel copies are one `rep movsb` on every CPU

The kernel's `memcpy`, `memmove` and `memset` are `compiler_builtins`', which
frame `rep movsq` or `rep stosq` with two byte strings, and take one `rep movsb`
or `rep stosb` only when built with the `ermsb` target feature
(`rust/library/compiler-builtins/compiler-builtins/src/mem/x86_64.rs:15-17,23-58,88-126`).
The kernel builds them with it, one path on every CPU: `rep movsb` is correct
without ERMS, CPUID.(7,0):EBX bit 9, and no CPU is refused. Zen 2 lacks ERMS
(a Ryzen 9 PRO 3900, family 0x17, reads EBX 0x219C91A9: InstLatx64 ddff8a92,
`AuthenticAMD/AuthenticAMD0870F10_K17_Matisse_CPUID.txt:52`), and no proving
machine is family 0x17, so the cost there is unmeasured.

**Exit**: a gate finds each forward copy and fill in `kernel.elf` one `rep
movsb` or `rep stosb`, with no `rep movsq` or `rep stosq`; the T14's pipe
figures. **Mutation**: the build without `ermsb` reds the gate. **Oracle**:
Linux's pipe figures.
