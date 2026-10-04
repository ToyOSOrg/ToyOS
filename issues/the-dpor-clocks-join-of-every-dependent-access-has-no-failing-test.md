---
status: assigned
kind: tooling
opened: 2026-09-28
---

# The DPOR clock's join of every dependent access has no failing test

ToyOSOrg/loom `toyos` `d63fbd07` (`src/rt/execution.rs:234`) joins every
access `dependent_accesses(operation)` yields into the DPOR clock, not just
the one it backtracks to. Joining only the first (`.take(1)`) survives
everything the fork's own suite and this tree's differential check for.

**Evidence:** with the join narrowed to `dependent_accesses(operation).take(1)`
(`m13.patch` in the review round's scratchpad), loom's own `cargo test` on the
fork exits 0, and a 400-program differential against a dependency-free
brute-force SC enumerator (2–3 threads, `SeqCst` loads, stores, `fetch_add`s,
CASes and swaps over one or two atomics) is byte-identical to the fork's own
run: the same outcome set and the same execution count in every program,
6,211,523 executions in total, `scmiss=0`.

**Exit condition:** a test that goes red under `m13.patch` and green on the
unpatched fork — for example a two-object program whose reachable execution
count under full joining differs from the count under `.take(1)`, since a
smaller clock only adds backtracking points and so cannot be told apart by an
outcome-set test. Owner: the author of the next change to
`src/rt/execution.rs`'s DPOR clock; held by the orchestrator.
