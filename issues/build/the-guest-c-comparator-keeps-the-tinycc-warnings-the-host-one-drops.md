---
status: expected-red
kind: tooling
opened: 2026-09-29
---

# The guest C comparator keeps the TinyCC warnings the host one drops, so `03_struct` reds on the T14

`check_c_result` (`tests/toyos.rs`) drops every `<case>.c:<n>: ... warning:`
line from a committed `.expect` before comparing, because TinyCC's runner
captured its warnings with the output. `ccheck`
(`tests/toyos-rust-tests/src/bin/ccheck.rs`), the metal corpus's comparator,
compares the staged `.expect` whole. `the_two_comparisons_use_one_rule`
holds the two to one `trim_end` and does not see the difference.
`tests/testcases/tinycc/03_struct.expect` opens with
`03_struct.c:14: warning: attribute '__cleanup__' ignored on type`, and it is
the one staged case whose expectation carries a warning.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1), boot `ccorpus`:

```
ccheck: 03_struct: differed at byte 0 — 25 byte(s) produced against 90 expected
ccheck: 03_struct: got      "12\n34\n12\n34\n56\n78\n~fred()"
ccheck: 03_struct: expected "03_struct.c:14: warning: attribute '__cleanup__' ignored on type\n12\n34\n12\n34\n56\n78\n~fred()"
[... cpu7] exit: 03_struct pid=12 code=1 cpu=62ms
```

The case binary itself exited 0 (`exit: test_c_03_struct pid=13 code=0`).

## Exit condition

The guest and host comparators apply one rule to an expectation's warning
lines, with the gate that holds them together reading that rule too, and a
T14 run of the corpus exits `03_struct` 0; then its row in `src/redlist.rs`
and this file are deleted.
