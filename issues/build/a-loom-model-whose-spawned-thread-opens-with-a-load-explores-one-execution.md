---
status: open
kind: tooling
opened: 2026-09-28
---

# A loom model whose spawned thread opens with a load explores one execution

Loom 0.7, as `kernel-loom` pins it, ran a model's closure **once** when the
spawned thread's first operation was a load: a bare loom `AtomicU64`, a
spawned thread doing two `fetch_add`s and the model's thread one `load`,
explored 10 executions; the same with a `Relaxed` load at the top of the
spawned thread explored 1. A reader spawned beside an adder on the model's own
thread explored 19. The probes were throwaway test files in `kernel-loom/tests/`
on `wt/toyos-schedule`, counting closure runs in a `std` atomic.

`kernel-loom/tests/i8042_tally.rs` was this shape — `Tally::record` opens with
its saturation check's load — so its two models checked no interleaving of
the real word, and a mutation that counted an empty interrupt as a carrying
one passed them. They now spawn the reader and refuse a model that ran one
execution (`explored`). No other model in `kernel-loom/` or
`toyos-sched/loom` has been counted.

**Exit**: every loom model in the tree shown to run more than one execution —
the same refusal `explored` makes, applied to each.
