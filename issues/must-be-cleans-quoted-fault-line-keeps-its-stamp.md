---
status: open
kind: tooling
opened: 2026-09-25
---

# `must_be_clean`'s quoted fault line keeps its stamp, so a reproduced failure reads as a different one

`tests/common/qemu.rs:539`'s `without_stamp` strips a kernel line's
`[kernel <t> cpu<N>] ` prefix so two runs of one deterministic panic compare
equal on the finding alone, stamp aside — the doc above it names exactly this:
"which of the two a verdict quotes decides an adjudication."

`Serial::must_be_clean` (`tests/common/serial.rs:356`) does not go through it.
Its message is `format!("{needle:?} on a {} that should not have it: {line:?}\n{}", ...)`
— `{line:?}` quotes the offending line whole, stamp included. A DMA fault's
line carries the guest's own simulated clock, which differs between an
in-parallel run and its lone re-run of the same defect. The alone-run
classifier compares these full strings and reports a `DIFFERENT failure` where
the fault is the same one, reproduced (`ctl-M7-noquiesce.log:974-976`, PR #484
round 5).

**Owed:** route `must_be_clean`'s (and `must_not_say`'s) quoted line through
`without_stamp` before it is compared or reported, or move `without_stamp`
somewhere both call sites reach.
