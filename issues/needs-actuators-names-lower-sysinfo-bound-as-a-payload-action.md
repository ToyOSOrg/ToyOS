---
status: open
kind: finding
opened: 2026-09-28
---

# `needs_actuators` names `LOWER_SYSINFO_BOUND` as a payload-carrying action

The comment in `tests/toyos.rs`'s `needs_actuators` lists `LOWER_SYSINFO_BOUND`
among the `SYS_DEBUG` actions that carry a payload and are reached through
`debug_with`. It takes no argument: `heap_ceiling.rs` reaches it through
`syscall::debug(LOWER_SYSINFO_BOUND)`, and the kernel's arm in
`kernel/src/syscall/dispatch.rs` reads no `a2`.

**Evidence:** `git grep -n "CENSUS_KIND, LOWER_SYSINFO_BOUND" origin/main -- tests/toyos.rs`.

**Exit condition:** the list names only actions that read an argument.
