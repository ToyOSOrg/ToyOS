---
status: open
kind: defect
opened: 2026-08-25
---

# Three green local clippys, three red hosted `host` jobs, one gap

Three times in two days a branch verified "clippy clean" locally was refused
by the hosted `host` job's clippy, each costing a full CI round-trip and once
an 80-minute self-heal loop re-arming a pull request whose required check
could never go green:

- #268 — `clippy::redundant_closure` (`writeback.rs`)
- #276 — `clippy::needless_borrow` (`tests/toyos.rs`)
- #283 — `clippy::manual_is_multiple_of` (`toyos-ld/tests/common/mod.rs`)

The gap is not the lint list, it is the invocation: agents run some variant of
`cargo clippy -p <crate>` or the kernel-only builds, while the `host` job runs
clippy across the whole workspace `--all-targets` (test targets included —
two of the three misses were in test code) under `-D warnings` with the
adopted lint set. A local run that does not match that invocation verifies a
different claim than the gate checks.

What would close it: one command in the tree that IS the host job's clippy
invocation — a `cargo run -- --clippy` (or a documented one-liner the workflow
itself calls, so the two cannot drift) — and the habit of running it before
every push lands in the place agents already read. The rate is measured: three
of the last three non-trivial branches.
