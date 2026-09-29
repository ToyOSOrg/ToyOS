---
status: open
kind: track
opened: 2026-09-29
---

# Kernel copies use `rep movsb` where the CPU has ERMS

The kernel's copies take `rep movsb` on a CPU that enumerates ERMS,
CPUID.(7,0):EBX bit 9, and the plain copy elsewhere; a CPU without ERMS is
never refused. Zen 2 has none: a Ryzen 9 PRO 3900 (CPUID.1:EAX 0x00870F10,
family 0x17) reads CPUID.(7,0):EBX 0x219C91A9, bit 9 clear (InstLatx64
ddff8a92, `AuthenticAMD/AuthenticAMD0870F10_K17_Matisse_CPUID.txt:52`), and
QEMU v11.1.1's EPYC-Rome model omits ERMS where its EPYC-Milan model carries
it (`target/i386/cpu.c:6792-6796,6895-6899`). The TCG model has no ERMS and
keeps the plain copy.

**Exit**: on every proving machine and under TCG the boot selects the copy
CPUID.(7,0):EBX bit 9 names; the T14's pipe figures. **Mutation**: a
selection that ignores the bit reds on one side. **Oracle**: the CPU's own
CPUID, and Linux's pipe figures.
