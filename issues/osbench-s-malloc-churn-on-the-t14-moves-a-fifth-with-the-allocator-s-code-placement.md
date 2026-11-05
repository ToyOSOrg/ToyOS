---
status: open
kind: tooling
opened: 2026-11-05
---

# osbench's malloc churn on the T14 moves a fifth with the allocator's code placement

`mem.malloc_churn.16.t1` and `.4096.t1` lost 18-21% on the T14 when #1150
grew `toyos::sync::lock_word`, which std's allocator never calls: its lock is
std's futex `Mutex`, which goes straight to `toyos_abi::syscall`
(`sdk/std/sys/pal/futex.rs`).
- **The shift:** the growth moved std's allocator code 0x720 bytes, which puts
  dlmalloc's `malloc` and `free`, `__rdl_alloc`, `__rdl_dealloc`, std's
  `dealloc` and `Mutex::lock_contended` 32 bytes off their earlier position
  modulo 64.
- **The proof:** 32 inert bytes beside `lock_word` put them back. That arm,
  which keeps the grown lock, reads base's values: 36514 and 36658 kops/s
  against base's 36586, and 28800 for the grown lock without the bytes
  (evidence `ToyOSOrg/evidence`, branch `pr-1150`, r13 for the symbol tables
  and r14 for the boots).

The AMD bench does not show it. So on this core a hot loop's throughput
depends on where an unrelated change leaves the allocator, and a benchmark
comparison across any change that resizes earlier code can report a fifth
of noise as a regression or a gain.

The mechanism inside the core is not established: which front-end structure
(the decoded-µop cache's 32-byte windows, or a branch boundary) the
placement crosses. No performance counter has been read.

**Exit:** the churn rows hold within run-to-run under a placement change: the
hot allocator functions aligned so that a shift of earlier code moves them by
whole aligned units, or osbench's comparison reports placement as its own
arm. Either is measured on the T14 with a padding arm like this one.
