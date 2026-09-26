---
status: open
kind: defect
opened: 2026-09-26
---

# A program's two streams map its one ring twice

Found in the review of PR #527. init puts one log ring in a program's slots 1
and 2, and `toyos/src/log/stdio.rs`'s `ask` takes each stream on its own: it
duplicates the slot's handle and maps the region, once for stdout and once for
stderr. So every program holds two handles and two 2 MiB mappings of the same
ring, within one instance of the SDK — before the second instance
`issues/design-debt/a-rust-program-holds-two-copies-of-its-stream-state.md`
counts.

Nothing a program can ask today tells it that two slots name one object, which
is why `ask` does not share the mapping: assuming it would be wrong for a child
whose parent put two different rings there.

**Exit condition**: one mapping per ring a process holds — the kernel
answering whether two handles name one object, or init handing the ring once
and the SDK binding both streams to it — shown by a guest test that counts the
process's mappings of its ring.
