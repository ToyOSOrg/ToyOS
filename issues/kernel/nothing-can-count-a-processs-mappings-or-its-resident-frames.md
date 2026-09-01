---
status: open
kind: track
opened: 2026-09-01
---

# Nothing can count a process's mappings or its resident frames, so no plateau can be measured

`issues/build/std-leaks-a-thread-stack-per-spawn.md` is the immediate consumer:
ToyOS std allocates a 2 MiB stack per spawn, stores only the thread id, and
`join` performs the join syscall and nothing else. Whether the mapping and its
frames come back is unanswerable from inside the tree, because no test-only
counter exposes either quantity per process.

**The technique already exists and is not wired to a number.**
`tests/toyos-rust-tests/src/bin/abuse_tls_alloc.rs` runs raw `thread_spawn` /
`thread_join` over an explicitly `mmap`ed 2 MiB stack, and its own comment names
this exact leak as the reason it avoids `std::thread::spawn`. So the missing
piece is not the harness shape — it is the observation. Anyone who writes "there
is no bounded observation for this" has skipped that file.

**What to build.** Test-only per-process mapping and resident-frame counts, then
bounded spawn/join plateaus driven through the *real* std path. A leak is a
plateau that keeps climbing; a fix is a plateau that returns. The plateau, not a
single before/after pair, is the verdict — a single pair cannot distinguish a
leak from a deferred release.

**The counter owes two things.** It must not allocate per mapping, and it must not
retain the objects it counts. Both are proved by comparing its total against a
quiescent page-table and PMM walk at the start and at the end.

**Half the defect is not the joined half.** A `JoinHandle` dropped without `join`
detaches, and the record's exit condition currently only closes the joined path.
The plateau must cover both.

**Reuse.** mmap, the loader, process retirement, and every future stack-lifetime
question want this counter.
