---
status: open
kind: tooling
opened: 2026-09-28
---

# A first dial turned away before a line is waited on to its caller's bound

`src/metaltalk.rs`'s `serve` never asks a first dial again when its
connection closes before a line (`again` is false), and it records no
`unopened` for it. So `Stream::wait_for_connection` still reads the stream as
dialling (`state.unopened.is_none()`) and waits out its whole `by`. Its doc says
it returns "once the dial asked for it ended with none". Through QEMU's
forward that close is the only sign the guest refused, and each caller then
names the wrong thing, late:

- `metalswap::swap` says "no boot opened the record stream within N s" once
  the whole window has passed.
- `tests/common/logstream.rs`'s `reader` says "the stream never opened" once
  `CEILING` has passed.
- `converse` waits `FOREVER` and returns only at the metal loop's `give_up`.

The close itself is recorded in `end`.

Measured: a mutation of
`a_refused_first_dial_is_asked_again_up_to_its_ceiling` hands its first dial
to a listener that accepts it and closes it before a line. That test's
`wait_connected(5 s)` returned `None` after 5.02 s, and `unopened()` was still
`None`.

## Exit condition

A first dial whose connection closes before a line ends `wait_for_connection`
at once, and `unopened` names the close. A host test stages that close
through a listener it owns and accepts on, and fails on the base.
