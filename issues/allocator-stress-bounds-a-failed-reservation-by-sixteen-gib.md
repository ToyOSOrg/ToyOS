---
status: open
kind: tooling
opened: 2026-09-29
---

# `allocator_stress` expects a 16 GiB reservation to fail, which a machine with more memory grants

`test_oom_graceful` (`tests/toyos-rust-tests/src/bin/allocator_stress.rs`)
asserts `try_reserve(16 * 1024 * 1024 * 1024)` is refused as "way more than
available RAM". That is a guest's size, as the total-memory range beside it
was: the shared metal boot runs the same binary on the T14, which reports
`mem_total=16777216000` (main's T14 run at `7e151819`, boot `shared`), 384 MiB
short of the reservation. A machine with more than 16 GiB free grants it and
reds the test.

## Exit condition

The refused size derives from the total `sysinfo` reports rather than from a
constant.
