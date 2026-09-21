---
status: open
kind: tooling
opened: 2026-09-21
---

# `kernel-loom`'s `dump_request` model has no must-red step in CI

Every other `kernel-loom` model has a cargo feature that removes the edge its
property rests on and a `host-tests.yml` step that fails if the model stays
green under it. `tests/dump_request.rs` has neither. Its teeth were shown once,
by hand, as checked patches on `kernel/src/sched/dump_request.rs` at `f78c147e`:
`take` ignoring a running report, the report's end taking nothing, and a request
met during a report consumed and dropped — `cargo test --test dump_request`
exited 101 under each, with `Concurrent write accesses to UnsafeCell`, `a
request is pending and no pass is obliged to take it`, and `two requests, the
second filed after the first was taken`.

Owed: one feature (the report's end taking nothing is the whole of what the
reporting bit bought), declared in `kernel/Cargo.toml` and `kernel-loom/Cargo.toml`,
listed in `src/build.rs`, and a must-red step naming
`a_request_filed_during_a_report_is_reported`.

Closed when that step is in `host-tests.yml` and red under the feature.
