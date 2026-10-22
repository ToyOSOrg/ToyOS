---
status: open
kind: defect
opened: 2026-10-21
---

# A stdio sink cached in one copy outlives a redirect in another

`toyos::log::stdio` asks once what descriptors 1 and 2 name and caches the
answer, a log ring's mapping or the descriptor itself, in a static of each
copy of `toyos` linked into a program (`SINKS`, `sdk/toyos/src/log/stdio.rs`).
The descriptor table is one per process (`toyos::fd`), but the cache is not:
libc's `dup2` onto 1 or 2 calls `forget` only in libc's own copy, so in a
program that mixes Rust and C, std's copy keeps writing to the ring it mapped
before the redirect. Std's own `toyos::fd::dup2` forgets nothing. A
descriptor sink is asked on every write and is unaffected; only a cached ring
goes stale. Read from the code; no guest case has shown it.

**Exit**: the sink cache lives with the table's owner, or every change of
descriptor 1 or 2 is seen by every copy, and a guest case redirects a ring
stdout from C and sees std's next line land at the new target.
