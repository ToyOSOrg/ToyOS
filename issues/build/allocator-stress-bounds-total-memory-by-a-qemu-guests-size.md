---
status: open
kind: tooling
opened: 2026-09-29
---

# `allocator_stress` bounds total memory by a QEMU guest's size, so the T14's 16 GB reds it

`test_memory_stats` (`tests/toyos-rust-tests/src/bin/allocator_stress.rs`)
asserts `sysinfo`'s total is 2 to 9 GB, because "QEMU is configured with
8GB". The shared metal boot runs the same binary on the T14.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1), boot `shared`, after every earlier check printed `ok`:

```
thread 'main' (1) panicked at src/bin/allocator_stress.rs:270:5:
mem_total=16777216000 (15 GB) out of range
[... cpu6] exit: test_rs_allocator_stress pid=46 code=101 cpu=92ms
```

## Exit condition

The bound derives from what the machine reports rather than from one
guest's size, and a T14 run of the shared boot exits `allocator_stress` 0; then this file is deleted.
