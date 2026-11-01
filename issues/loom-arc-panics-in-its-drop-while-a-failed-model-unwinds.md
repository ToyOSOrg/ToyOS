---
status: open
kind: tooling
opened: 2026-10-12
---

# Loom's `Arc` panics in its drop while a failed model unwinds

The loom fork, ToyOSOrg/loom `toyos` at `dfe3797`, carries v0.7.2's
`src/sync/arc.rs` unchanged. `Arc`'s `Drop` calls `rt::Arc::ref_dec`, which
branches the scheduler. A model that deadlocks has no thread active when
`Execution::schedule` panics, so the drop that panic's unwinding reaches
unwraps `None` in `rt::thread::Set::active_mut` (`src/rt/thread.rs:276`), and
a destructor panicking during cleanup aborts the process. The test binary
prints loom's `deadlock; threads = ...`, then aborts before the harness prints
the test's `FAILED` line or its summary: a gate that reads the harness's
verdicts finds none. Loom's cells and atomics skip their checks
while `std::thread::panicking()`; its `Arc` does not.

**Evidence** ([ToyOSOrg/evidence `29c8ebcb`, `arc-probe/`](https://github.com/ToyOSOrg/evidence/tree/29c8ebcb04eae318598ff7cb0a045e6e9a1c4f53/arc-probe)):
at `6d2e78400`, with the stream model in
`sdk/transport/tests/loom.rs` on `loom::sync::Arc` in place of
std's, `cargo test -p toyos-transport --features want-relaxed --test loom --
a_consumer_that_looked_again_is_rung_for_bytes_it_missed --exact` kills the
test binary with SIGABRT, `panic in a destructor during cleanup`, through
`<loom::sync::arc::Arc<Stream> as Drop>::drop` and `rt::arc::Arc::ref_dec`.
On std's `Arc` the same run prints the test's `FAILED` line and loom's
`deadlock; threads = ...`. That model is now `sdk/toyos/tests/transport_loom.rs`,
its control `transport-want-relaxed` of `toyos`.

**Owner**: the orchestrator. Nobody holds it.

**Exit**: a test on the fork's `toyos` branch in which a model that deadlocks
with a `loom::sync::Arc` live prints loom's `deadlock` message and its test
fails without an abort, and that stream model and `lib/link/tests/ends_loom.rs`
hold their state in loom's `Arc`.
