---
status: open
kind: defect
opened: 2026-09-29
---

# User programs run without a shadow stack

At `Ubuntu-6.8.0-142.142`, `CONFIG_X86_USER_SHADOW_STACK=y` (config line 502)
lets a process opt into CET shadow stacks: a return address a `RET` pops is
checked against a hardware-maintained copy that ordinary writes cannot reach,
so a corrupted return address on the data stack is refused rather than taken.
ToyOS builds no user shadow stack: no program can enable one, and nothing in
`kernel/src` maps a shadow-stack page or sets `CR4.CET`/`MSR_IA32_U_CET`.

**Exit**: a process can have a shadow stack of its own, the kernel maps and
enables one for it, and a `RET` to a return address forged on the data stack
is refused rather than taken.
