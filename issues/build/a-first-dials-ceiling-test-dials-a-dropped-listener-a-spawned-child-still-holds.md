---
status: open
kind: tooling
opened: 2026-09-28
---

# A first dial's ceiling test dials a dropped listener that a spawned child still holds

`metaltalk::tests::a_refused_first_dial_is_asked_again_up_to_its_ceiling`
binds `127.0.0.1:0`, drops the listener, and expects every dial to that
address to be refused. That holds only while no other thread in the test
process is spawning a child. A spawned child holds a copy of every fd until
its exec closes the close-on-exec ones, and while it does, the dropped
listener still completes handshakes. Many lib tests spawn processes.

When one of the stream's first dials is taken, the connection closes before
a line once the child's exec closes the listener. A first dial does not ask
again after that, so `unopened` stays `None`. The test then reds on "the dial
gave up within 5 s of its 60 and said why".

Evidence, in the job scratchpad `redial-r3/`:
- `l3-staged-109.log`: this red in the full `--lib` suite at `8cc19ef1`, 1 of
  200 full-suite runs, at one-minute load average 42.2, beside
  `cargo test --workspace --exclude toyos-build`. `941f0bda`'s test binary,
  run interleaved with it, reddened here twice in its 200.
- `fdprobe/`: bind, drop, connect, 20000 times per run. With no concurrent
  spawn, 0 were taken in each of two runs. With a thread spawning children
  beside it, 34 were taken and 4 reset, then 26 taken and 7 reset.

PR #566 raised this test's dials from 3 to `TURNED_AWAY_CEILING` (64), which
widens the time its dials span. `a_redial_on_a_forward_that_refuses_ends_at_the_refusal`
had the same shape and now stages its refusal through `Reach`.

## Exit condition

The test's refusals come from a staged `Reach` rather than a dropped
listener. It is shown green across at least 200 full `--lib` suite runs beside
`cargo test --workspace --exclude toyos-build`.
