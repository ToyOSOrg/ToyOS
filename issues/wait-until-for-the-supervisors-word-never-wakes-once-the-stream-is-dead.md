---
status: open
kind: tooling
opened: 2026-09-28
---

# `wait_until` for init's final word never wakes once the stream is dead

`Stream::wait_until` (`src/metaltalk.rs:260`) is woken by each line as it
lands, and by nothing else. `wait_for_connection` (`src/metaltalk.rs:289`)
computes its own `dialing` — `redial.is_some() || current.is_some() ||
unopened.is_none()` — and returns `None` at once once a redial has ended with
no connection: the stream can never carry another line. `metalswap::swap`'s
call to `wait_until` for init's final word (`src/metalswap.rs:214`) has no
such exit, so it waits out its whole `window` even after the stream has
reached that same dead state.

Predates this branch: a redial that ends without naming a cause already left
`wait_until` waiting before `src/metaltalk.rs:383` started ending a forward's
redial at its first refusal.

Evidence: hold-red took 124 s this way, run at PR #566's `7e06a657`
(`566r2-hold-red.log:507`, orchestrator's round-2 review job).

## Exit condition

Give `wait_until` the same exit `wait_for_connection` already has: return
`None` once the stream's own `dialing` state goes false, instead of waiting
out `by`.
