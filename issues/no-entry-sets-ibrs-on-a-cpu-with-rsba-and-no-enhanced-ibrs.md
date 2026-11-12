---
status: open
kind: defect
severity: [security]
opened: 2026-11-12
---

# No entry sets IBRS on a CPU with RSBA and no enhanced IBRS

On an Intel CPU with RSBA behavior and no enhanced IBRS,
`kernel::mitigation::decide` names "IBRS on entry" for spectre_v2 and
retbleed: Intel's "Return Stack Buffer Underflow" has IBRS, not retpoline,
hold a return that underflows the RSB there. Nothing sets it, so every entry
from Ring 3 runs with `IA32_SPEC_CTRL.IBRS` as the CPU's bring-up left it,
clear. The guest runner's CPU is one such: it enumerates IBRS
(CPUID.(07H,0):EDX[26]) and no `IBRS_ALL`, and its signature is in
"Retpoline"'s Table 5. No proving machine is.

**Exit**: each entry from Ring 3 sets `IBRS` in `IA32_SPEC_CTRL` before its
first indirect branch, and each return to Ring 3 writes back the value the
thread runs with; a CPU whose decision does not name it runs no added
instruction, chosen per CPU as the branch-history clear is; and idle holds
what Intel's "Speculative Execution Side Channel Mitigations" asks of `IBRS`
before a sleep state on such a CPU. On the guest runner a probe reads `IBRS`
set inside a syscall and the thread's own value after its return.
**Mutation**: the return's write deleted. **Oracle**: the CPU's own register.
