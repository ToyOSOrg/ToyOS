---
status: open
kind: defect
opened: 2026-09-29
---

# User programs have no protection keys

`CR4.PKE` is never set and no mapping carries a key, so a thread cannot fence
part of its own memory off from itself. Linux at `Ubuntu-6.8.0-142.142` builds
with `X86_INTEL_MEMORY_PROTECTION_KEYS`
(`debian.master/config/annotations:15334`). Its ABI is approved in principle
and shaped in this issue's pull request, which lands after
`issues/kernel/user-programs-use-avx-under-xsave.md`, since PKRU is XSAVE
state.

**Exit**: under TCG with `+pku` (`target/i386/cpu.c:992` at QEMU v11.1.1), on
the T14, and on each EPYC guest that enumerates PKU: a write under a key the
thread denied is `#PF` with PK set, and a sibling thread's is not; `window`
refuses a `read(2)` into a page under a key the thread write-denied, and a
`write(2)` from a buffer under a key it access-denied. **Mutation**: no key in
the entry; one PKRU for all threads; `window` without the key check; `window`
checking the key only on `Access::Write`, which the `write(2)` reds.
**Oracle**: TCG and the T14 on SDM Vol. 3A §5.6.2.
