---
status: open
kind: tooling
opened: 2026-10-27
---

# The AMD bench's Linux reads its clock through a syscall

On 2026-10-24 `osbench`'s `time.now`, `Instant::now` a million times a
round, read 1,212,895 ps a call under the AMD bench's Ubuntu (Linux
6.8.0-147, both runs within 0.1%) against 14,302 ps under the T14's, the same
static binary. A read that slow is a clocksource the vDSO cannot read, so
each one is a syscall: an inference from the cost, since that session did
not record the clocksource. ToyOS reads the TSC on both benches, so every
per-operation timing of the AMD bench's Linux carries some 1.2 µs that its
ToyOS runs do not.

osbench now records `clocksource` with Linux's conditions, and
`tests/guest/osbench/README.md` runs a comparison with `clocksource=tsc`.

**Exit**: the AMD bench's Linux reads `clocksource=tsc` in an `osbench` run,
and its `time.now` is within twice the T14's.

**Owner**: the orchestrator, which runs the benches. Changing the bench's
Linux boot configuration is the owner's decision.
