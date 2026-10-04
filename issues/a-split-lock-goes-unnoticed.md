---
status: open
kind: defect
opened: 2026-09-29
---

# A split lock goes unnoticed

The kernel never sets `MSR_TEST_CTRL` (0x33) bit 29, so a user `lock`
instruction across a cache line locks the bus for every CPU and nothing
reports it. Linux at `Ubuntu-6.8.0-142.142` detects it by default and warns
(`sld_warn`, `arch/x86/kernel/cpu/intel.c:1083`); ToyOS ends the process by
name instead.

**Exit**: on the T14, the one proving machine known to enumerate split-lock
detection, a `lock add` across a line is ended by name. **Mutation**: bit 29
clear. **Oracle**: the T14's CPU.
