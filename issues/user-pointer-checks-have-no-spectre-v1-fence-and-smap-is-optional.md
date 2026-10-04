---
status: open
kind: defect
opened: 2026-09-29
---

# User-pointer checks have no Spectre v1 fence, and SMAP is optional

`user_ptr`'s range checks carry no `lfence` and no index mask, and SMAP is an
optional bit of the CR4 declaration
(`kernel/src/arch/x86_64/control_regs.rs:60`), so a CPU without it boots, and
a mispredicted check can load through a user pointer. Linux at
`Ubuntu-6.8.0-142.142` reports `usercopy/swapgs barriers and __user pointer
sanitization`.

**Exit**: SMAP is required and a boot without it is refused by name; on every
proving machine a gadget through each `user_ptr` site recovers a planted byte
no more often than chance, 1 in 256, by a bound the stage derives over 1000
trials from the same gadget's unfenced rate on that machine. **Mutation**:
SMAP optional again; a site's fence or mask removed exceeds the bound.
**Oracle**: the CPU's own speculation.
