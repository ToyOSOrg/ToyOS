---
status: open
kind: tooling
opened: 2026-09-28
---

# A redial's bound test assumes its reader dials within fifty milliseconds

`metaltalk::tests::a_redial_ends_at_its_bound_alone_and_says_so` redials with
a 50 ms bound. Its closed-before-a-line arm asserts that the reason names
"redial's bound" and "ended before a line", so the reader must dial at least
once inside those 50 ms. Under load the reader thread can wake after the
bound has passed. `open` then never dials and says so: `At(127.0.0.1:52453)
was not serving its log by the bound: never asked`. That answer is correct
for a redial whose bound passed before a dial. The test's red is its own
assumption about the host's scheduler.

Seen once in 200 full `--lib` suite runs of `941f0bda`'s test binary, at
one-minute load average 19.7, sampled after the run, beside `cargo test --workspace --exclude
toyos-build`: `l3-prefix-25.log` in the job scratchpad `redial-r3/`. The test
is the same at `8cc19ef1`, which was green in the same 200 runs.

## Exit condition

The arm's first dial is an event the test waits on, not one it expects inside
a wall-clock bound. It is shown green across at least 200 full `--lib` suite
runs beside `cargo test --workspace --exclude toyos-build`.
