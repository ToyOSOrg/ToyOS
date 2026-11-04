---
status: open
kind: defect
severity: [security]
opened: 2026-09-29
---

# User-pointer checks have no Spectre v1 fence

`user_ptr`'s range checks carry no `lfence` and no index mask, so a
mispredicted check can load through a user pointer. Linux at
`Ubuntu-6.8.0-142.142` reports `usercopy/swapgs barriers and __user pointer
sanitization`.

**Exit**: on every proving machine a gadget through each `user_ptr` site
recovers a planted byte no more often than chance, 1 in 256, by a bound the
stage derives over 1000 trials from the same gadget's unfenced rate on that
machine. **Mutation**: a site's fence or mask removed exceeds the bound.
**Oracle**: the CPU's own speculation.
