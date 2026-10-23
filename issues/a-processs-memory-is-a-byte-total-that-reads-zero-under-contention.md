---
status: open
kind: track
opened: 2026-09-01
---

# A process's memory is a byte total that reads zero under contention, so no plateau can be trusted

Deciding whether a process's mappings and their frames come back means
reading a *plateau* — a sequence that climbs and returns — and that is what this
tree cannot do honestly today. std's thread stacks did without one: a stack
still allocated cannot share a base with the next, so `std_threads` reads
reuse instead.

**Three things already exist, and this track is the difference between them and
a plateau.**

`SYS_SYSINFO` carries a live per-process byte total: the private memory its
space has populated (`Usage::resident_bytes`, `kernel/src/mm/space.rs`), read
in `kernel/src/syscall/machine.rs`'s roster. The usage is atomics, but it is
reached through the process's data **under `try_lock`, and the failure arm
writes `0`**. A plateau reads a sequence; a spurious zero in that sequence is
indistinguishable from a release, and it appears exactly when the process is
busy — which is when a spawn/join loop is running. Objects the process made,
its TLS blocks and regions among them, are in no part of it.

`ProcessStats` (`SYS_PROCESS_STATS`) carries `peak_memory` and
`alloc_count`, which is the ranges the space has made (`Usage::ranges_made`,
`kernel/src/mm/space.rs`); the ranges taken away are counted beside it
(`Usage::ranges_taken`) and logged at exit, but not exported. Neither is a
live count of memory: a range is any run of pages, so one `munmap` can take
part of one and another can take several, so exporting the other count
yields no number that returns to where it was. `peak_memory` is a high-water mark, which means a
*fix* is invisible in it: a leak that stops leaking leaves the peak where it was.

`tests/guest/rust-tests/src/bin/abuse_tls_alloc.rs` has the harness shape. It
runs raw `thread_spawn`/`thread_join` over one explicitly `mmap`ed stack, so
the address space holds still while it measures. What it does not do is read a
number.

**So what is missing is narrow and worth building.** A test-only per-process
*count* of live mappings and resident frames — counts, not bytes — read without
`try_lock`, so a sample is either a number or an explicit refusal and never a
silent zero. Then bounded spawn/join plateaus driven through the real std path.

**The counter owes two things.** It must not allocate per mapping, and it must
not retain the objects it counts. Both are proved by comparing its total against
a quiescent page-table and PMM walk at the start and at the end.

Accounting is a pillar of `issues/toyos-explains-itself.md`.
