---
status: open
kind: defect
opened: 2026-09-29
---

# No kernel address is drawn per boot

The direct map sits at the constant `PHYS_OFFSET`
(`kernel/src/mm/mod.rs:33`) and the kernel image in it at the physical
address firmware allocated (`bootloader/src/arch/x86_64.rs:90`), so nothing
but firmware moves the kernel's layout. Linux at `Ubuntu-6.8.0-142.142` builds
with `RANDOMIZE_BASE` and `RANDOMIZE_MEMORY`
(`debian.master/config/annotations:10674,10677`): at most 9 bits of text slot,
and a direct map placed at PUD granularity.

**Exit**: on every proving machine, over 64 boots in a test,
each claimed bit is set 12 to 52 times, the guard of
`issues/kernel/the-kernel-has-no-stack-protector.md` and the first spawn's
bases included. **Mutation**: a fixed base. **Oracle**: Linux's bits.
