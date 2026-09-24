---
status: open
kind: defect
opened: 2026-09-25
---

# The stop's no-lost-wake claim rests on x86's locked read-modify-writes

`kernel/src/quiesce.rs`'s `stop` arms `PROGRESS` before its first sweep, and
says a transition landing between a sweep and the park after it leaves a record
that the park returns on at once. The two sides of that claim are a
store-buffering pair:

- **The poster** (`note_progress`) makes its transition, which is a CAS on the
  task's state word, and then `completion::post_with` loads `Watch::armed`
  `Relaxed` and returns without a post if it reads zero
  (`kernel/src/completion/mod.rs`, `post_with`).
- **The caller** stores `armed` inside `completion::arm`, and then its sweep
  loads each state word after an `Acquire` lock RMW on `PROCESS_TABLE`.

Under the Rust memory model nothing orders either side's store before its
load, so both sides may read the old value. The poster then sees no arm and
posts nothing, and the sweep sees the thread still running. The stop sleeps out
`quiesce::PARK` and its record reads as a stop that nothing woke. On x86 every
locked instruction is a full fence, so the pair cannot both read stale values
there. The pattern is inherited from `post_with`, which every completion shares.
The stop is what depends on it for a claim of its own.

Nothing models it: `kernel-loom` has no model of `arm`/`post_with` against a
caller that reads a second location.

**Exit condition**: before this kernel runs on ARM64, a loom model of the arm,
the transition and the sweep. Either the model shows no lost wake under the
orderings the code uses, or the orderings are strengthened until it does.
