---
status: open
kind: tooling
opened: 2026-09-30
---

# A metal failure drops every row its boot measured

`tests/common/metal.rs`'s `judge_readbacks` puts boot labels in `failed`, and
`Readback::measured` records a number with no test attached. One red test or
shared member riding a boot therefore keeps every number that boot measured
out of the record, and a number never recorded is never judged. A runner that
carries every test in one session would drop every row of the session for one
red test.

It already costs rows. On the T14's full run at `0d2dda66f`, `testcases`,
`shared`, `lancase` and `lantalkcase` each carried a known red, and
`tests/metal/lenovo-20w0003amz.toml` holds no row of theirs, so nothing
judges their numbers while those reds stand.

## Exit condition

Each number is recorded against its owner, and only that owner fails.
