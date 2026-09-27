---
status: open
kind: defect
opened: 2026-09-27
---

# A block publish pays for a wake nobody asks for

Every `Producer::publish` in `toyos-transport` stores its tail, runs a `SeqCst`
fence and loads the consumer's `sleep` word, so that it can answer
`Wake::Peer` or `Wake::Busy`. blockd (`userland/blockd/src/main.rs`,
`publish`) and its client (`userland/blockd/src/session.rs`, `pump`) ring the
connection on every publish whatever the answer, and neither end calls
`Consumer::before_sleep`, so the answer is always `Busy`: the fence and the
load are paid once per batch and buy nothing. Their cost has not been
measured.

**Exit condition.** blockd and its client sleep through `before_sleep` and
ring their peer only on `Wake::Peer`.
