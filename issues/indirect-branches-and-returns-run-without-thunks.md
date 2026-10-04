---
status: open
kind: defect
opened: 2026-09-29
---

# Indirect branches and returns run without thunks

Nothing in the kernel's build asks for a retpoline, a return thunk or
straight-line-speculation padding. Linux at `Ubuntu-6.8.0-142.142` builds with
`MITIGATION_RETPOLINE`, `MITIGATION_RETHUNK`, `MITIGATION_ITS` and `SLS`
(`debian.master/config/annotations:8043-8045,12190`) and installs the return
thunk its decision selects: ITS's on the T14; on AMD, retbleed's untrained
return (`arch/x86/kernel/cpu/bugs.c:1125`) or SRSO's safe RET, in its alias
form on family 0x19 (`bugs.c:2726-2732`).

**Exit**: a gate over `kernel.elf`, on the decoder of
`issues/no-gate-decodes-kernel-elfs-instructions.md`, finds no raw
indirect branch or `ret` outside the thunks and entry, an `int3` after each, and every placement Linux's linker
script asserts (`arch/x86/kernel/vmlinux.lds.S:510-539`):
`retbleed_return_thunk` and `srso_safe_ret` at a line start, the SRSO alias
pair's addresses differing in exactly bits 2, 8, 14 and 20, and each ITS
thunk's branch in its line's upper half. Each proving machine's boot installs
the thunks the decision selects for it: ITS's on the T14, SRSO's alias on a
family-0x19 EPYC guest. No proving machine is family 0x17, so the untrained
return is proven by the gate and the decision's host test alone.
**Mutation**, each red: an ITS thunk at a line start; `retbleed_return_thunk`
one byte past its line start; the SRSO alias one cache line off its pair; an
`int3` gone; ITS's thunk installed on every CPU. **Oracle**: that linker
script, and `Documentation/admin-guide/hw-vuln/indirect-target-selection.rst`.
