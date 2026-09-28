---
status: open
kind: tooling
opened: 2026-09-28
---

# A refused first dial gives up past its test's five seconds under host load

`src/metaltalk.rs`'s `a_refused_first_dial_is_asked_again_up_to_its_ceiling`
failed once in 200 full runs of the toyos-build lib test binary at `9aa473d0`,
run beside a `cargo test --workspace --exclude toyos-build` loop as host load
(1-minute load average 29.32 at that run):

```
panicked at src/metaltalk.rs:1326:37:
the dial gave up within 5 s of its 60 and said why
```

The test gives three refused dials 5 s of wall clock (`wait_connected(5 s)`)
to reach the ceiling. Under that load, the third dial had not been turned away
by then. The other 199 runs passed.

No host-test counterpart of `src/redlist.rs` disables it, so it still runs in
every `cargo test -p toyos-build --lib`.

**Exit**: the test waits on the ceiling being reached rather than on a
wall-clock bound, or the bound is shown to have a margin that host load cannot
eat.
