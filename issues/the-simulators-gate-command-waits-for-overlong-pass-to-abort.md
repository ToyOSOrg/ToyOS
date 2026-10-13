---
status: open
kind: tooling
opened: 2026-10-12
---

# `kernel-sim gate` waits for `overlong_pass` to abort, and it records instead

`kernel/sim/src/main.rs`'s `gate` runs `overlong_pass` through
`sweep::abort_gate` and prints `NOT CAUGHT — the harness proves nothing`
unless the core aborts. `scenarios::overlong_pass`'s own doc says the
opposite: "It must be recorded, not aborted" — the pass-cost recorder
(`cpu::PassCosts`) replaced the assert, and `sim/tests/scenarios.rs`'s
`a_pass_that_overruns_its_budget_is_recorded` holds the recording. So the
command that `kernel/sim/src/lib.rs` names as the full exit criterion exits 1
on a tree whose every gate holds.

Measured on `main` at `177e72a9e`: `kernel-sim gate 10` prints
`overlong_pass: NOT CAUGHT` and every other negative gate caught; PR #874's
head prints the same at `gate 10000`.

**Exit**: `kernel-sim gate` judges `overlong_pass` by its recorded pass costs,
as the test does, and exits 0 on a tree whose gates hold.
