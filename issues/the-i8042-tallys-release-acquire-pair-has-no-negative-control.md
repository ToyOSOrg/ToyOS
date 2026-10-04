---
status: assigned
kind: tooling
opened: 2026-09-28
---

# The i8042 tally's release/acquire pair has no negative control

`kernel/src/arch/x86_64/i8042/tally.rs` publishes an interrupt's count with a
`Release` `fetch_add` in `record` and reads it with an `Acquire` load in `read`,
so a reader that sees a count sees the bytes behind it. No row of `src/ci.rs`'s
`CONTROLS` weakens that pair, so nothing shows that
`kernel-loom/tests/i8042_tally.rs` would catch its loss.

**Evidence:** with both orderings weakened to `Relaxed`, under the loom fork,
`cargo test -p kernel-loom --test i8042_tally` exits 101:
`a_counted_interrupt_carries_its_bytes_with_it` panics with "a reader counted
an interrupt as having delivered a byte and could not see the byte", and the
other two models pass.

**Exit condition:** a `kernel-loom` feature that weakens the pair to `Relaxed`,
declared like the crate's other controls and run from `CONTROLS` with
`a_counted_interrupt_carries_its_bytes_with_it ... FAILED` as its verdict.
Owner: the author of the next change to `tally.rs` or `i8042_tally.rs`; held by
the orchestrator.
